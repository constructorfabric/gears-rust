//! Deterministic derivation of the usage-record identity.
//!
//! The record `id` is not an independent field: it is a deterministic
//! projection of the dedup key `(tenant_id, gts_id, idempotency_key, created_at)`
//! (ADR-0014). The gateway derives it on every create; a client MAY reproduce
//! the same value locally with this function (e.g. to reference a not-yet-acked
//! record via `corrects_id` without a round-trip), provided it supplies the same
//! `created_at` (at microsecond precision).

use time::OffsetDateTime;
use uuid::Uuid;

use crate::models::{IdempotencyKey, UsageTypeGtsId};

/// Fixed namespace for deterministic usage-record `id` derivation (`UUIDv5`).
///
/// NEVER change this value: changing it re-maps every dedup key to a new
/// `id`, breaking idempotency and every stored `corrects_id` reference.
pub const USAGE_RECORD_ID_NAMESPACE: Uuid =
    Uuid::from_u128(0x5631_3026_863b_4de8_b32b_1f96_b673_06ed);

/// ASCII unit separator between the dedup-key fields. `tenant_id`
/// (hex + hyphen), `gts_id` (GTS grammar), and `created_at_micros` (ASCII
/// digits, optional leading `-`) cannot contain it, and `idempotency_key` is
/// the final field (consumes the remainder), so the encoding is injective even
/// when a key itself contains `0x1F`.
const FIELD_SEPARATOR: u8 = 0x1F;

/// Derive the deterministic record id from the 4-tuple dedup key:
/// `id = UUIDv5(NS, tenant_id ⟨0x1F⟩ gts_id ⟨0x1F⟩ created_at_micros ⟨0x1F⟩ idempotency_key)`,
/// where `tenant_id` is its canonical lowercase-hyphenated string form,
/// `gts_id` / `idempotency_key` are their UTF-8 bytes, and `created_at_micros`
/// is the event timestamp as integer microseconds-since-epoch (decimal ASCII).
///
/// `created_at` is canonicalized to microseconds — the precision Postgres
/// `timestamptz` stores and the active plugin dedups on — so a sub-microsecond
/// difference between an original submission and its retry cannot derive a
/// different id (which would surface as a false `IdempotencyConflict`). The
/// canonicalization is the shared [`created_at_micros`] primitive.
#[must_use]
pub fn derive_usage_record_id(
    tenant_id: Uuid,
    gts_id: &UsageTypeGtsId,
    idempotency_key: &IdempotencyKey,
    created_at: OffsetDateTime,
) -> Uuid {
    let micros = created_at_micros(created_at);

    let mut input = Vec::new();
    input.extend_from_slice(tenant_id.to_string().as_bytes());
    input.push(FIELD_SEPARATOR);
    input.extend_from_slice(gts_id.as_ref().as_bytes());
    input.push(FIELD_SEPARATOR);
    input.extend_from_slice(micros.to_string().as_bytes());
    input.push(FIELD_SEPARATOR);
    input.extend_from_slice(idempotency_key.as_str().as_bytes());
    Uuid::new_v5(&USAGE_RECORD_ID_NAMESPACE, &input)
}

/// Canonical projection of an event timestamp to its integer
/// microseconds-since-epoch count — the single source of truth for the µs
/// canonicalization the identity contract relies on.
///
/// Postgres `timestamptz` stores microsecond precision, so both the derived
/// [`derive_usage_record_id`] and the active plugin's dedup-equality check MUST
/// project `created_at` through *this* function: two timestamps that differ only
/// below microsecond precision project to the same value (and thus the same id /
/// the same dedup key), so an exact retry never surfaces as a false
/// `IdempotencyConflict`. Keeping the projection here — rather than re-deriving
/// `unix_timestamp() * 1_000_000 + microsecond()` per crate — is what keeps a
/// future precision change from silently diverging the two.
///
/// Built from the whole-second unix timestamp plus the sub-second microsecond
/// component (no integer division), so it is exact for pre-epoch (negative
/// unix-timestamp) instants too.
#[must_use]
pub fn created_at_micros(created_at: OffsetDateTime) -> i128 {
    i128::from(created_at.unix_timestamp()) * 1_000_000 + i128::from(created_at.microsecond())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "id_tests.rs"]
mod id_tests;
