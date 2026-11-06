//! Deterministic derivation of the ledger-entry identity.
//!
//! The entry `id` is not an independent field: it is a deterministic
//! projection of the dedup identity
//! `(tenant_id, gts_type_id, idempotency_key, window_start, window_end, entry_type)`
//! (`cpt-cf-usage-collector-adr-record-identity-derivation`). The
//! Ingestion Gateway derives it at one choke point for every surface, and an
//! emitter reproduces the same value offline. From the measurement it sent
//! it reproduces two identifiers: that measurement's, and the one a
//! withdrawal of it would carry. The gateway resolves a withdrawal's target
//! by the same function over the withdrawal's own inputs, which is why no
//! caller-supplied reference is needed — or accepted: a submission naming
//! its own target is refused.

use thiserror::Error;
use time::{OffsetDateTime, UtcOffset};
use uuid::Uuid;

use crate::v2::models::{EntryType, IdempotencyKey, MeterTypeId};

/// Fixed namespace for the entry-identity derivation (`UUIDv5`).
///
/// NEVER change this value: it admits no rotation, no per-deployment value
/// and no versioned variant, because each of those re-maps every identifier
/// the gear has ever issued.
pub const USAGE_RECORD_ID_NAMESPACE: Uuid =
    Uuid::from_u128(0x5631_3026_863b_4de8_b32b_1f96_b673_06ed);

/// ASCII unit separator between the dedup-identity fields.
///
/// The concatenation stays injective only while no input carries this byte.
/// Four of the six cannot carry it by construction: a UUID, two
/// fixed-width timestamps, and a closed enumeration rendering one of two
/// ASCII literals ([`EntryType::as_str`]). The other two arrive as
/// caller-supplied *strings*, and both newtypes reject every ASCII control
/// character ([`MeterTypeId::new`], [`IdempotencyKey::new`]) — which is what
/// keeps two distinct dedup identities from concatenating to one pre-image.
const FIELD_SEPARATOR: u8 = 0x1F;

/// Renders a covered-period bound in the canonical 27-character form
/// `YYYY-MM-DDTHH:MM:SS.ffffffZ`.
///
/// The form is frozen with the namespace constant: a change to the fraction
/// width, the case of `T` / `Z`, or the text encoding re-maps every
/// identifier the gear has issued. Six digits is the microsecond, and it is
/// the precision ceiling of the derivation — a caller that sends
/// `12:00:00Z`, `12:00:00.000Z` or `13:00:00+01:00` reaches one canonical
/// form here.
///
/// A bound finer than the microsecond is **refused**, not truncated. Six
/// digits is the precision ceiling, so a seventh carries information the
/// pre-image cannot hold: truncating it would answer one identifier for two
/// periods a nanosecond apart, and every ingestion path rejects both
/// (`sub_microsecond_period_bound`). In-gear the refusal is unreachable —
/// [`crate::v2::CreateUsageRecord::try_into_usage_record`] and
/// [`crate::v2::CreateUsageRecord::try_into_invalidation_record`] check the
/// precision before the derivation runs — but the rendering is exported so
/// an independent implementation can reproduce the pre-image, and **that**
/// caller has no other way to honour the obligation. It gets the refusal
/// rather than a plausible wrong UUID.
///
/// The 27-character width holds for a year in `0..=9999`, and a bound
/// outside it is refused rather than rendered. Every bound that arrives over
/// REST is RFC 3339-parsed, so the width is guaranteed on the wire path. An
/// in-process caller can construct a year down to `-9999`, which `{:04}`
/// renders as `-001` — still 27 characters, but not the canonical form.
///
/// # Errors
///
/// [`PeriodBoundInvalid::YearOutOfRange`] outside that range, and
/// [`PeriodBoundInvalid::SubMicrosecond`] for a bound carrying a non-zero
/// nanosecond remainder.
pub fn canonical_period_bound(bound: OffsetDateTime) -> Result<String, PeriodBoundInvalid> {
    let utc = bound.to_offset(UtcOffset::UTC);
    let year = utc.year();
    if !(0..=9999).contains(&year) {
        return Err(PeriodBoundInvalid::YearOutOfRange { year });
    }
    // `UtcOffset` holds whole seconds, so the nanosecond component is
    // invariant under the normalization above and this reads the same value
    // `require_microsecond_precision` does on the ingestion path.
    let nanosecond = utc.nanosecond();
    if !nanosecond.is_multiple_of(1_000) {
        return Err(PeriodBoundInvalid::SubMicrosecond { nanosecond });
    }
    Ok(format!(
        "{year:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:06}Z",
        u8::from(utc.month()),
        utc.day(),
        utc.hour(),
        utc.minute(),
        utc.second(),
        utc.microsecond(),
    ))
}

/// Why a covered-period bound could not be rendered in the canonical form.
///
/// Carried by [`canonical_period_bound`] alone, which previously answered a
/// non-canonical string in release builds and panicked in debug ones.
#[derive(Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum PeriodBoundInvalid {
    /// The bound's UTC year falls outside the `0..=9999` the fixed
    /// 27-character form holds.
    #[error(
        "a covered-period bound renders as the fixed 27-character \
         `YYYY-MM-DDTHH:MM:SS.ffffffZ`, whose year component holds 0..=9999; got {year}"
    )]
    YearOutOfRange {
        /// The bound's year, after normalization to UTC.
        year: i32,
    },

    /// The bound carries finer than microsecond precision, which the fixed
    /// six-digit fraction cannot hold.
    ///
    /// Refused rather than truncated: two bounds a nanosecond apart would
    /// otherwise render one canonical form and derive one identifier, while
    /// every ingestion path refuses both.
    #[error(
        "a covered-period bound renders as the fixed 27-character \
         `YYYY-MM-DDTHH:MM:SS.ffffffZ`, whose six-digit fraction is the microsecond; got a \
         nanosecond component of {nanosecond}, which is not a whole number of microseconds"
    )]
    SubMicrosecond {
        /// The bound's nanosecond component, after normalization to UTC.
        nanosecond: u32,
    },
}

/// Why an entry identity could not be derived.
///
/// Both are caller obligations: deriving over an inverted period or a
/// non-canonical bound would answer an identifier the published derivation
/// does not define, so neither can be papered over. Reporting them rather than
/// asserting lets an emitter reproducing an identity offline handle the
/// refusal, and spares every call site a guard of its own.
#[derive(Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum UsageRecordIdInvalid {
    /// The covered period runs backwards.
    #[error(
        "a covered period runs from window_start to window_end: got window_start {window_start}, \
         which is after window_end {window_end}"
    )]
    PeriodInverted {
        /// The inclusive start the caller supplied.
        window_start: OffsetDateTime,
        /// The exclusive end the caller supplied.
        window_end: OffsetDateTime,
    },

    /// The inclusive start does not render in the canonical form.
    ///
    /// Split from [`Self::WindowEnd`] rather than carrying one bound-shaped
    /// variant, so a caller reporting the refusal names the field the caller
    /// actually sent wrong instead of guessing between the two.
    #[error("window_start is not a canonical covered-period bound: {0}")]
    WindowStart(#[source] PeriodBoundInvalid),

    /// The exclusive end does not render in the canonical form.
    #[error("window_end is not a canonical covered-period bound: {0}")]
    WindowEnd(#[source] PeriodBoundInvalid),
}

/// Derives the entry identity from the 6-tuple dedup identity:
/// `id = UUIDv5(NS, tenant_id ⟨0x1F⟩ gts_type_id ⟨0x1F⟩ idempotency_key ⟨0x1F⟩ window_start ⟨0x1F⟩ window_end ⟨0x1F⟩ entry_type)`.
///
/// `tenant_id` enters in its lowercase hyphenated 36-character form,
/// `gts_type_id` and `idempotency_key` byte-exact (terminator `~`
/// included), both bounds in the canonical form
/// [`canonical_period_bound`] renders, and `entry_type` as its lowercase
/// ASCII wire literal ([`EntryType::as_str`]), `record` or `invalidation`.
/// `NS` enters as its 16 raw bytes, per RFC 4122.
///
/// A point event derives over a zero-length period, where the two bounds
/// are equal; the derivation needs no separate case for it.
///
/// Entry type is the last input, and it is what tells a measurement from
/// its withdrawal. An invalidation copies the tenant, type, idempotency key
/// and covered period of its target, so the two differ in this input alone
/// — and the same function over the invalidation's own fields with
/// `entry_type = record` gives the identifier of the entry it withdraws,
/// which is how the gateway resolves the target. Two withdrawals of one
/// target collide on all six inputs, so every invalidation of one entry
/// derives one identifier. `reason_code` is no input here, so two
/// withdrawals departing only in it reach one identity and meet at full
/// canonical equality (`cpt-cf-usage-collector-adr-mandatory-idempotency`).
///
/// # Errors
///
/// [`UsageRecordIdInvalid::WindowStart`] / [`UsageRecordIdInvalid::WindowEnd`]
/// when that bound has no canonical rendering — outside
/// [`canonical_period_bound`]'s year range, or finer than microsecond
/// precision — and [`UsageRecordIdInvalid::PeriodInverted`] when
/// `window_start` is after `window_end`.
///
/// **The bounds are judged before their order**, so a call wrong in both ways
/// names the bound: a bound that has no canonical rendering is the more
/// specific fault. Order-first would also cost coverage on the year arm,
/// where the two faults are not independent — `time` is built without
/// `large-dates`, so the only out-of-range years are negative, and an end
/// below the range with `window_start <= window_end` puts the start below it
/// too, leaving [`UsageRecordIdInvalid::WindowEnd`] reachable on precision
/// alone.
pub fn derive_usage_record_id(
    tenant_id: Uuid,
    gts_type_id: &MeterTypeId,
    idempotency_key: &IdempotencyKey,
    window_start: OffsetDateTime,
    window_end: OffsetDateTime,
    entry_type: EntryType,
) -> Result<Uuid, UsageRecordIdInvalid> {
    // Both render before either is pushed, so a rejected bound cannot leave
    // a half-built pre-image behind.
    let rendered_start =
        canonical_period_bound(window_start).map_err(UsageRecordIdInvalid::WindowStart)?;
    let rendered_end =
        canonical_period_bound(window_end).map_err(UsageRecordIdInvalid::WindowEnd)?;
    if window_start > window_end {
        return Err(UsageRecordIdInvalid::PeriodInverted {
            window_start,
            window_end,
        });
    }
    let (window_start, window_end) = (rendered_start, rendered_end);

    let mut input = Vec::new();
    input.extend_from_slice(tenant_id.to_string().as_bytes());
    input.push(FIELD_SEPARATOR);
    input.extend_from_slice(gts_type_id.as_str().as_bytes());
    input.push(FIELD_SEPARATOR);
    input.extend_from_slice(idempotency_key.as_str().as_bytes());
    input.push(FIELD_SEPARATOR);
    input.extend_from_slice(window_start.as_bytes());
    input.push(FIELD_SEPARATOR);
    input.extend_from_slice(window_end.as_bytes());
    input.push(FIELD_SEPARATOR);
    input.extend_from_slice(entry_type.as_str().as_bytes());
    Ok(Uuid::new_v5(&USAGE_RECORD_ID_NAMESPACE, &input))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "id_tests.rs"]
mod id_tests;
