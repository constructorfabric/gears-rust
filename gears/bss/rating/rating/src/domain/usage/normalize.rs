//! `UsageNormalizer` (feature 04 §3.2; DESIGN [12 §4.1]): a feed entry as the usage-collector v2
//! record shapes it → the normalized columns of its `bss_rating__usage_record` row, or a rejection.
//! Pure: the caller resolved the page's meter declarations before its transaction.
//!
//! The checks are the collector's own bounds (`usage-collector-sdk/src/v2/models.rs`,
//! `quantity.rs`). A conforming collector never sends an entry that fails them; Rating checks
//! because it captures whatever arrives and must never count what it cannot read.
//!
//! [12 §4.1]: ../../../../docs/DESIGN.md#contract-12-4-1

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde_json::{Map, Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::domain::usage::declarations::PageDeclarations;
use crate::domain::usage::entry::EntryIdentity;
use crate::domain::usage::feed::{MeterId, is_refused_char};

/// The longest `resource_ref` / `subject_ref` component, in characters.
pub const MAX_ATTRIBUTION_LEN: usize = 256;
/// The longest `idempotency_key`, in characters.
pub const MAX_IDEMPOTENCY_KEY_LEN: usize = 256;
/// The longest `reason_code`, in characters.
pub const MAX_REASON_CODE_LEN: usize = 128;
/// The most significant digits of a quantity.
pub const MAX_QUANTITY_SIGNIFICANT_DIGITS: usize = 28;
/// The most integer or fractional digits of a quantity.
const MAX_QUANTITY_PART_DIGITS: usize = 28;

/// Whether an entry is a measurement or a withdrawal. The SDK record carries no discriminator: an
/// entry is an invalidation exactly when it carries `invalidates`.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntryType {
    /// A measurement.
    Record,
    /// A withdrawal of the record `invalidates` names.
    Invalidation,
}

impl EntryType {
    /// The stored name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Record => "record",
            Self::Invalidation => "invalidation",
        }
    }

    /// The type a stored name denotes.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        [Self::Record, Self::Invalidation]
            .into_iter()
            .find(|kind| kind.as_str() == name)
    }
}

/// Which collector route admitted the entry.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Origin {
    /// The live ingestion path.
    Live,
    /// The backfill route; processed identically.
    Backfill,
}

impl Origin {
    /// The stored name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Backfill => "backfill",
        }
    }

    /// The origin a stored name denotes.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        [Self::Live, Self::Backfill]
            .into_iter()
            .find(|origin| origin.as_str() == name)
    }
}

/// A quantity as the collector sent it: its canonical decimal text (at most 28 significant and 28
/// fractional digits, no negative zero). Kept as text, because the collector's equality is textual
/// (`42.5` ≠ `42.500`); summed exactly downstream.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Quantity(String);

impl Quantity {
    /// A quantity in the collector's pattern `^-?(0|[1-9][0-9]{0,27})(\.[0-9]{1,28})?$`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let unsigned = text.strip_prefix('-').unwrap_or(text);
        let (integer, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
        let digits = |part: &str| part.bytes().all(|b| b.is_ascii_digit());
        let integer_ok = !integer.is_empty()
            && integer.len() <= MAX_QUANTITY_PART_DIGITS
            && digits(integer)
            && (integer == "0" || !integer.starts_with('0'));
        let fraction_ok = !unsigned.contains('.')
            || (!fraction.is_empty()
                && fraction.len() <= MAX_QUANTITY_PART_DIGITS
                && digits(fraction));
        if !integer_ok || !fraction_ok {
            return None;
        }
        let significant = integer
            .bytes()
            .chain(fraction.bytes())
            .skip_while(|b| *b == b'0')
            .count();
        let negative_zero = text.starts_with('-') && significant == 0;
        (significant <= MAX_QUANTITY_SIGNIFICANT_DIGITS && !negative_zero)
            .then(|| Self(text.to_owned()))
    }

    /// The text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The withdrawal an invalidation carries.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct InvalidationRef {
    /// `invalidates`: the target's id, derived by the collector.
    pub target: Uuid,
    /// `reason_code`.
    pub reason_code: String,
}

/// What an entry is: a measurement, or the withdrawal of the record it names.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EntryKind {
    /// A measurement.
    Record,
    /// A withdrawal: `invalidates` and `reason_code`.
    Invalidation(InvalidationRef),
}

impl EntryKind {
    /// The stored `entry_type`.
    #[must_use]
    pub const fn entry_type(&self) -> EntryType {
        match self {
            Self::Record => EntryType::Record,
            Self::Invalidation(_) => EntryType::Invalidation,
        }
    }
}

/// `subject_ref`: who consumed the usage, lineage only.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UsageSubject {
    /// `subject_id`.
    pub subject_id: String,
    /// `subject_type`, when the entry names one.
    pub subject_type: Option<String>,
}

impl UsageSubject {
    /// The stored `subject_ref`: the two validated fields and nothing else the entry carried.
    #[must_use]
    pub fn to_json(&self) -> Value {
        match &self.subject_type {
            Some(subject_type) => {
                json!({"subject_id": self.subject_id, "subject_type": subject_type})
            }
            None => json!({"subject_id": self.subject_id}),
        }
    }
}

/// The normalized columns of a captured entry.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedUsage {
    /// Its identity.
    pub identity: EntryIdentity,
    /// `gts_type_id`: the meter, the record's raw input usage type (T-D-76).
    pub gts_type: MeterId,
    /// `idempotency_key`.
    pub idempotency_key: String,
    /// `resource_ref.resource_type`.
    pub resource_type: String,
    /// `resource_ref.resource_id`.
    pub resource_id: String,
    /// `subject_ref`, its validated fields (lineage only).
    pub subject: Option<UsageSubject>,
    /// `window_start`.
    pub interval_start: OffsetDateTime,
    /// `window_end`.
    pub interval_end: OffsetDateTime,
    /// `quantity`.
    pub quantity: Quantity,
    /// `metadata`: string values under declared keys (empty when absent).
    pub metadata: BTreeMap<String, String>,
    /// A record, or an invalidation with its target; derived from `invalidates`.
    pub kind: EntryKind,
    /// `origin`.
    pub origin: Origin,
    /// `accepted_at`.
    pub accepted_at: OffsetDateTime,
}

impl NormalizedUsage {
    /// The stored `entry_type`.
    #[must_use]
    pub const fn entry_type(&self) -> EntryType {
        self.kind.entry_type()
    }

    /// The withdrawal an invalidation carries; `None` on a record.
    #[must_use]
    pub const fn invalidation(&self) -> Option<&InvalidationRef> {
        match &self.kind {
            EntryKind::Invalidation(reference) => Some(reference),
            EntryKind::Record => None,
        }
    }

    /// The stored `metadata` object.
    #[must_use]
    pub fn metadata_json(&self) -> Value {
        Value::Object(
            self.metadata
                .iter()
                .map(|(key, value)| (key.clone(), Value::String(value.clone())))
                .collect(),
        )
    }

    /// The collector's dedup identity, Rating's defence key (12 §4.3).
    #[must_use]
    pub fn natural_key(&self) -> NaturalKey {
        NaturalKey {
            tenant_id: self.identity.tenant_id,
            gts_type: self.gts_type.clone(),
            idempotency_key: self.idempotency_key.clone(),
            interval_start: self.interval_start,
            interval_end: self.interval_end,
            entry_type: self.entry_type(),
        }
    }
}

/// `uq_bss_rating__usage_record__natural_key`.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NaturalKey {
    /// `tenant_id`.
    pub tenant_id: Uuid,
    /// `gts_type`.
    pub gts_type: MeterId,
    /// `idempotency_key`.
    pub idempotency_key: String,
    /// `interval_start`.
    pub interval_start: OffsetDateTime,
    /// `interval_end`.
    pub interval_end: OffsetDateTime,
    /// `entry_type`.
    pub entry_type: EntryType,
}

/// Why an entry is `rejected` (feature 04 §4.5 exceptions).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RejectReason {
    /// The entry is not of the v2 shape, names an undeclared meter or undeclared metadata keys.
    UsageMalformed,
    /// The entry has no covered period.
    IntervalMissing,
    /// Another stored entry holds its natural key (a safety net, 12 §4.3).
    NaturalKeyConflict,
}

impl RejectReason {
    /// Every reason, in a stable order.
    pub const ALL: [Self; 3] = [
        Self::UsageMalformed,
        Self::IntervalMissing,
        Self::NaturalKeyConflict,
    ];

    /// The stored reason and exception reason code.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UsageMalformed => "usage_malformed",
            Self::IntervalMissing => "interval_missing",
            Self::NaturalKeyConflict => "usage_natural_key_conflict",
        }
    }

    /// The reason a stored name denotes.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|reason| reason.as_str() == name)
    }

    /// The `reason` label of `rating_usage_rejected_total`.
    #[must_use]
    pub const fn metric_label(self) -> &'static str {
        match self {
            Self::UsageMalformed => "malformed",
            Self::IntervalMissing => "interval_missing",
            Self::NaturalKeyConflict => "natural_key_conflict",
        }
    }
}

/// A failed interpretation: the reason and what was wrong (no other tenant's data).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejection {
    /// Why.
    pub reason: RejectReason,
    /// What, for the exception's detail.
    pub detail: String,
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.reason.as_str(), self.detail)
    }
}

fn malformed(detail: impl Into<String>) -> Rejection {
    Rejection {
        reason: RejectReason::UsageMalformed,
        detail: detail.into(),
    }
}

/// Interprets `entry` (stored under `identity`) with the page's meter `declarations`.
///
/// # Errors
///
/// The [`Rejection`] the entry is stored with.
pub fn normalize(
    entry: &Value,
    identity: EntryIdentity,
    declarations: &PageDeclarations,
) -> Result<NormalizedUsage, Rejection> {
    // @cpt-begin:cpt-cf-bss-rating-algo-intake-normalize:p1:inst-norm-map
    let object = entry
        .as_object()
        .ok_or_else(|| malformed("the entry is not a JSON object"))?;
    let meter = text(object, "gts_type_id", 512)?;
    let gts_type = MeterId::parse(meter).map_err(|error| malformed(error.to_string()))?;
    let (interval_start, interval_end) = covered_period(object)?;
    let kind = match invalidation(object)? {
        Some(reference) => EntryKind::Invalidation(reference),
        None => EntryKind::Record,
    };
    if let Some(stated) = object.get("entry_type")
        && stated.as_str() != Some(kind.entry_type().as_str())
    {
        return Err(malformed("entry_type disagrees with invalidates"));
    }
    let quantity = object
        .get("quantity")
        .and_then(Value::as_str)
        .and_then(Quantity::parse)
        .ok_or_else(|| malformed("quantity is not a v2 decimal string"))?;
    let resource = object
        .get("resource_ref")
        .and_then(Value::as_object)
        .ok_or_else(|| malformed("resource_ref is missing"))?;
    let resource_id = text(resource, "resource_id", MAX_ATTRIBUTION_LEN)?.to_owned();
    let resource_type = text(resource, "resource_type", MAX_ATTRIBUTION_LEN)?.to_owned();
    let subject = subject_ref(object)?;
    let idempotency_key = text(object, "idempotency_key", MAX_IDEMPOTENCY_KEY_LEN)?.to_owned();
    let origin = object
        .get("origin")
        .and_then(Value::as_str)
        .and_then(Origin::parse)
        .ok_or_else(|| malformed("origin is neither live nor backfill"))?;
    let accepted_at =
        instant(object, "accepted_at")?.ok_or_else(|| malformed("accepted_at is missing"))?;
    // @cpt-end:cpt-cf-bss-rating-algo-intake-normalize:p1:inst-norm-map
    // @cpt-begin:cpt-cf-bss-rating-algo-intake-normalize:p1:inst-norm-malformed
    // @cpt-begin:cpt-cf-bss-rating-algo-intake-normalize:p1:inst-norm-fold
    // Only a definitive "not declared" rejects; any fold is accepted (12 §4.7, R-06).
    let declaration = declarations
        .get(&gts_type)
        .cloned()
        .flatten()
        .ok_or_else(|| malformed("the meter is not declared"))?;
    // @cpt-end:cpt-cf-bss-rating-algo-intake-normalize:p1:inst-norm-fold
    let metadata = metadata(object, &declaration.metadata_keys)?;
    // @cpt-end:cpt-cf-bss-rating-algo-intake-normalize:p1:inst-norm-malformed
    // @cpt-begin:cpt-cf-bss-rating-algo-intake-normalize:p1:inst-norm-dimension
    // The meter stays the record's raw input usage type; no dimension value is derived from
    // metadata at launch (R-16).
    Ok(NormalizedUsage {
        identity,
        gts_type,
        idempotency_key,
        resource_type,
        resource_id,
        subject,
        interval_start,
        interval_end,
        quantity,
        metadata,
        kind,
        origin,
        accepted_at,
    })
    // @cpt-end:cpt-cf-bss-rating-algo-intake-normalize:p1:inst-norm-dimension
}

/// A required string of 1 to `max_chars` characters without an ASCII control character (the
/// collector's rule, [`is_refused_char`]).
fn text<'a>(
    object: &'a Map<String, Value>,
    field: &str,
    max_chars: usize,
) -> Result<&'a str, Rejection> {
    let value = object
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| malformed(format!("{field} is missing or not a string")))?;
    if value.is_empty() || value.chars().count() > max_chars || value.chars().any(is_refused_char) {
        return Err(malformed(format!(
            "{field} must be 1 to {max_chars} characters without ASCII control characters"
        )));
    }
    Ok(value)
}

/// An RFC 3339 instant at microsecond precision, or `None` when the field is absent.
fn instant(object: &Map<String, Value>, field: &str) -> Result<Option<OffsetDateTime>, Rejection> {
    let Some(value) = object.get(field).filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let parsed = value
        .as_str()
        .and_then(|text| OffsetDateTime::parse(text, &Rfc3339).ok())
        .ok_or_else(|| malformed(format!("{field} is not an RFC 3339 instant")))?
        .to_offset(time::UtcOffset::UTC);
    if !parsed.nanosecond().is_multiple_of(1_000) {
        return Err(malformed(format!("{field} is finer than a microsecond")));
    }
    Ok(Some(parsed))
}

fn covered_period(
    object: &Map<String, Value>,
) -> Result<(OffsetDateTime, OffsetDateTime), Rejection> {
    let start = instant(object, "window_start")?;
    let end = instant(object, "window_end")?;
    let (Some(start), Some(end)) = (start, end) else {
        return Err(Rejection {
            reason: RejectReason::IntervalMissing,
            detail: "window_start and window_end are required".to_owned(),
        });
    };
    if end < start {
        return Err(malformed("window_end precedes window_start"));
    }
    Ok((start, end))
}

fn invalidation(object: &Map<String, Value>) -> Result<Option<InvalidationRef>, Rejection> {
    let invalidates = object.get("invalidates").filter(|value| !value.is_null());
    let reason_code = object.get("reason_code").filter(|value| !value.is_null());
    match (invalidates, reason_code) {
        (None, None) => Ok(None),
        (Some(target), Some(_)) => {
            let target = target
                .as_str()
                .and_then(|text| Uuid::parse_str(text).ok())
                .filter(|id| !id.is_nil())
                .ok_or_else(|| malformed("invalidates is not an entry id"))?;
            let reason_code = text(object, "reason_code", MAX_REASON_CODE_LEN)?.to_owned();
            Ok(Some(InvalidationRef {
                target,
                reason_code,
            }))
        }
        _ => Err(malformed(
            "invalidates and reason_code are both present or both absent",
        )),
    }
}

/// `subject_ref`'s two fields, validated; any other member the entry carries stays in `raw_entry`
/// only.
fn subject_ref(object: &Map<String, Value>) -> Result<Option<UsageSubject>, Rejection> {
    let Some(subject) = object.get("subject_ref").filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let fields = subject
        .as_object()
        .ok_or_else(|| malformed("subject_ref is not an object"))?;
    let subject_id = text(fields, "subject_id", MAX_ATTRIBUTION_LEN)?.to_owned();
    let subject_type = if fields
        .get("subject_type")
        .is_some_and(|value| !value.is_null())
    {
        Some(text(fields, "subject_type", MAX_ATTRIBUTION_LEN)?.to_owned())
    } else {
        None
    };
    Ok(Some(UsageSubject {
        subject_id,
        subject_type,
    }))
}

/// `metadata` closed to the meter's declared keys, each value a string without NUL. The collector
/// leaves values otherwise opaque (`reject_nul_metadata_values`), so Rating bounds them no tighter:
/// the entry as a whole is bounded by [`crate::domain::usage::entry::MAX_RAW_ENTRY_BYTES`].
fn metadata(
    object: &Map<String, Value>,
    declared: &BTreeSet<String>,
) -> Result<BTreeMap<String, String>, Rejection> {
    let Some(metadata) = object.get("metadata").filter(|value| !value.is_null()) else {
        return Ok(BTreeMap::new());
    };
    let entries = metadata
        .as_object()
        .ok_or_else(|| malformed("metadata is not an object"))?;
    let mut values = BTreeMap::new();
    for (key, value) in entries {
        if !declared.contains(key) {
            return Err(malformed("a metadata key is not declared for the meter"));
        }
        let text = value
            .as_str()
            .ok_or_else(|| malformed("a metadata value is not a string"))?;
        if text.contains('\0') {
            return Err(malformed("a metadata value carries a NUL"));
        }
        values.insert(key.clone(), text.to_owned());
    }
    Ok(values)
}

#[cfg(test)]
#[path = "normalize_tests.rs"]
mod tests;
