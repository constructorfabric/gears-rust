//! The pieces of the browse text narrowing that belong to the handler: what a
//! cursor is bound to, which rows the value half of a match admits, and how
//! wide a candidate batch is.

use crate::api::rest::setting_dto::EffectiveValueDto;
use crate::domain::search::{Needle, text_projection};
use crate::infra::storage::declaration_repo::DECLARATION_LIMIT_CFG;

/// The smallest candidate batch: a page of a handful of rows still reads a
/// batch worth the round trip.
const MIN_CANDIDATE_BATCH: u64 = 50;

/// The largest candidate batch — the declaration page maximum, which the
/// repository clamps to anyway.
const MAX_CANDIDATE_BATCH: u64 = DECLARATION_LIMIT_CFG.max;

/// The page size the repository cuts for a requested `limit`: the default
/// when absent, at least one, at most the declaration page maximum.
#[must_use]
pub(crate) fn page_size(requested: Option<u64>) -> u64 {
    requested
        .unwrap_or(DECLARATION_LIMIT_CFG.default)
        .clamp(1, DECLARATION_LIMIT_CFG.max)
}

/// How many candidates one batch reads for a page of `limit` rows: wider than
/// the page, so a page that the text thins out is refilled in few rounds.
#[must_use]
pub(crate) fn candidate_batch(limit: u64) -> u64 {
    limit
        .saturating_mul(4)
        .clamp(MIN_CANDIDATE_BATCH, MAX_CANDIDATE_BATCH)
}

/// FNV-1a, 64 bits: a public specification with fixed constants, so a token
/// minted by one release is read by the next — which `DefaultHasher`, whose
/// algorithm is unspecified, does not promise.
fn fnv1a_64(parts: &[&[u8]]) -> u64 {
    const BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01B3;
    let mut hash = BASIS;
    for part in parts {
        for &byte in *part {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(PRIME);
        }
    }
    hash
}

/// The token a narrowed browse's cursors are bound to: the `$filter` hash the
/// toolkit would bind them to, and the trimmed needle. The same filter and the
/// same text mint the same token; a cursor carrying another — or none, as one
/// minted without `q` does — is refused, so a page never continues under a
/// text it was not cut under. Not a security boundary, only a guard against
/// continuing one listing with another's cursor.
#[must_use]
pub(crate) fn browse_binding(filter_hash: Option<&str>, needle: &Needle) -> String {
    // A marker byte tells an absent filter hash from an empty one, and a
    // separator keeps the two parts from sliding into each other.
    let (marker, hash): (&[u8], &[u8]) = match filter_hash {
        Some(hash) => (b"\x01", hash.as_bytes()),
        None => (b"\x00", b""),
    };
    format!(
        "{:016x}",
        fnv1a_64(&[marker, hash, b"\xff", needle.as_str().as_bytes()])
    )
}

/// Whether the value half of the match admits a row: its `value` or its
/// `fallback`, as the caller receives them, contains the needle without regard
/// to letter case. A masked value — a `secret`, or a `pii` value the caller
/// may not read unmasked — never matches, so that no match discloses it, nor
/// does a handle a reclassified declaration still holds (`secret_backed`); a
/// JSON `null` never matches, as the search leaves a `null` Schema Default
/// out; a structured value matches on its leaves, through its JSON text.
#[must_use]
pub(crate) fn value_part_matches(
    dto: &EffectiveValueDto,
    secret_backed: bool,
    needle: &Needle,
) -> bool {
    // @cpt-begin:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-11
    let shown =
        |value: &serde_json::Value| !value.is_null() && needle.matches(&text_projection(value));
    !dto.masked && !secret_backed && (shown(&dto.value) || shown(&dto.fallback))
    // @cpt-end:cpt-cf-settings-service-flow-value-resolution-admin-browse:p1:inst-vr-browse-11
}

#[cfg(test)]
#[path = "browse_narrowing_unit_tests.rs"]
mod browse_narrowing_unit_tests;
