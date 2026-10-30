//! The pieces of the browse text narrowing that belong to the handler: what a
//! cursor is bound to, which rows the value half of a match admits, and how
//! wide a candidate batch is.

use std::hash::Hasher as _;

use uuid::Uuid;

use crate::api::rest::setting_dto::EffectiveValueDto;
use crate::domain::search::{Needle, text_projection};
use crate::infra::storage::declaration_repo::DECLARATION_LIMIT_CFG;

/// The smallest candidate batch: a page of a handful of rows still reads a
/// batch worth the round trip.
const MIN_CANDIDATE_BATCH: u64 = 50;

/// The largest candidate batch — the declaration page maximum, which the
/// repository clamps to anyway.
const MAX_CANDIDATE_BATCH: u64 = DECLARATION_LIMIT_CFG.max;

/// Ends a part of the bytes a binding hashes: never a byte of the hex filter
/// hash, of a tenant id's text or of UTF-8 text, so the parts never slide
/// into each other.
const BINDING_SEPARATOR: &[u8] = b"\xff";

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

/// The token a browse's cursors are bound to: the `$filter` hash the toolkit
/// would bind them to, the target tenant, and the trimmed needle when there is
/// one. The same filter, target and text mint the same token; a cursor
/// carrying another — or none — is refused, so a page never continues under a
/// filter, a target or a text it was not cut under: which rows a page holds
/// depends on all three. Not a security boundary — the target is gated on
/// every request — only a guard against continuing one listing with another's
/// cursor.
///
/// Hashed by FNV-1a, 64 bits: a public specification with fixed constants, so
/// a token minted by one release is read by the next — which `DefaultHasher`,
/// whose algorithm is unspecified, does not promise. The bytes go in through
/// `write`, never through `Hash`, whose encodings are not promised either.
#[must_use]
pub(crate) fn browse_binding(
    filter_hash: Option<&str>,
    target: Uuid,
    needle: Option<&Needle>,
) -> String {
    let mut hasher = fnv::FnvHasher::default();
    // No filter binds as an empty filter hash, and no `q` as an empty text:
    // neither narrows the page.
    hasher.write(filter_hash.unwrap_or_default().as_bytes());
    hasher.write(BINDING_SEPARATOR);
    let mut buffer = Uuid::encode_buffer();
    hasher.write(target.hyphenated().encode_lower(&mut buffer).as_bytes());
    hasher.write(BINDING_SEPARATOR);
    hasher.write(needle.map_or("", Needle::as_str).as_bytes());
    format!("{:016x}", hasher.finish())
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
