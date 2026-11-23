//! @cpt-dod:cpt-cf-bss-products-dod-quota-catalog-contract:p1
//! The canonical rendering and the domain-separated digests of the quota catalog (P-D-267), in
//! the SDK so that an adapter computes the same bytes Products checks.
//!
//! # The rendering
//!
//! One JSON rendering, the SDK's own rule: object keys sorted by their bytes at every depth, no
//! whitespace, strings escaped as JSON escapes them and otherwise verbatim, integers bare and
//! other numbers as `serde_json` prints them, arrays in the order given. Python's
//! `json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)` gives the same
//! text for the strings and integers these records carry.
//!
//! # The observation time
//!
//! The snapshot digest hashes `observed_at` as RFC 3339 text in UTC: `Z` as the offset, whole
//! seconds always written, and a fraction of a second only when it is not zero, without trailing
//! zeros (`2026-11-13T12:00:00Z`, `2026-11-13T12:00:00.25Z`). An adapter that sends this form
//! may hash the text it sent; any other form of the same instant (`+00:00`, `.000`, a local
//! offset) must be rewritten to it first.
//!
//! # The digests
//!
//! A digest is the lowercase hex SHA-256 of a domain line and the rendering,
//! `domain + "\n" + rendering`, one domain per kind, so that two kinds never share a digest for
//! the same content. Registry evidence hashes the rendering alone. Each digest is taken once, at
//! its write; no read computes one again.
use std::fmt;

use serde_json::{Map, Value};
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};
use toolkit_security::crypto::hash::Sha256;
use uuid::Uuid;

/// The domain of a whole capability snapshot.
pub const SNAPSHOT_DOMAIN: &str = "cf.bss.products.quota_capability_snapshot.v1";
/// The domain of one capability record.
pub const RECORD_DOMAIN: &str = "cf.bss.products.quota_capability.v1";
/// The domain of a definition version's content.
pub const DEFINITION_DOMAIN: &str = "cf.bss.products.quota_definition.v1";
/// The domain of a mapping version's content.
pub const MAPPING_DOMAIN: &str = "cf.bss.products.quota_mapping.v1";
/// The domain of a qualified profile version's content.
pub const PROFILE_DOMAIN: &str = "cf.bss.products.quota_profile.v1";

const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

/// Render `value` canonically (see the module documentation).
#[must_use]
pub fn render(value: &Value) -> String {
    let mut out = String::new();
    render_into(value, &mut out);
    out
}

fn render_into(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        Value::Number(number) => out.push_str(&number.to_string()),
        Value::String(text) => push_string(text, out),
        Value::Array(items) => {
            out.push('[');
            for (position, item) in items.iter().enumerate() {
                if position > 0 {
                    out.push(',');
                }
                render_into(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => render_object(map, out),
    }
}

fn render_object(map: &Map<String, Value>, out: &mut String) {
    // Sorted here rather than trusted from the map: its order depends on whether `serde_json`'s
    // `preserve_order` feature is on anywhere in the build.
    let mut entries: Vec<(&String, &Value)> = map.iter().collect();
    entries.sort_unstable_by(|left, right| left.0.cmp(right.0));
    out.push('{');
    for (position, (key, entry)) in entries.into_iter().enumerate() {
        if position > 0 {
            out.push(',');
        }
        push_string(key, out);
        out.push(':');
        render_into(entry, out);
    }
    out.push('}');
}

/// One string, escaped the way JSON escapes strings, through `Value`'s infallible `Display`.
fn push_string(text: &str, out: &mut String) {
    out.push_str(&Value::String(text.to_owned()).to_string());
}

/// The lowercase hex SHA-256 of `domain + "\n" + rendering`; an empty `domain` hashes the
/// rendering alone, as registry evidence does.
#[must_use]
pub fn sha256_hex(domain: &str, rendering: &str) -> String {
    let mut hash = Sha256::new();
    if !domain.is_empty() {
        hash.update(domain);
        hash.update("\n");
    }
    hash.update(rendering);
    let digest = hash.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(HEX_DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

/// The canonical digest of one complete capability snapshot: its id, revision, observation time
/// (in the text form of the module documentation), whole-snapshot count and every record sorted
/// by its `id`. It excludes the page cursor and the snapshot's own digest field.
///
/// # Errors
/// [`DigestError::ItemWithoutId`] when a record has no string `id`;
/// [`DigestError::InstantOutOfRange`] when `observed_at` has no UTC instant or no RFC 3339 form.
pub fn snapshot_digest(
    snapshot_id: Uuid,
    source_revision: &str,
    observed_at: OffsetDateTime,
    item_count: u64,
    items: &[Value],
) -> Result<String, DigestError> {
    let mut keyed = Vec::with_capacity(items.len());
    for item in items {
        let id = item
            .get("id")
            .and_then(Value::as_str)
            .ok_or(DigestError::ItemWithoutId)?;
        keyed.push((id, item));
    }
    keyed.sort_by(|left, right| left.0.cmp(right.0));
    // `checked_to_offset`, not `to_offset`: an instant valid in its own offset can leave the
    // representable range once moved to UTC, and `to_offset` panics there.
    let observed_at = observed_at
        .checked_to_offset(UtcOffset::UTC)
        .ok_or(DigestError::InstantOutOfRange)?
        .format(&Rfc3339)
        .map_err(|_| DigestError::InstantOutOfRange)?;
    let snapshot = serde_json::json!({
        "snapshot_id": snapshot_id.to_string(),
        "source_revision": source_revision,
        "observed_at": observed_at,
        "item_count": item_count,
        "items": keyed.into_iter().map(|(_, item)| item.clone()).collect::<Vec<_>>(),
    });
    Ok(sha256_hex(SNAPSHOT_DOMAIN, &render(&snapshot)))
}

/// The digest of a registry entity's content, kept as intake evidence: the rendering alone.
#[must_use]
pub fn content_digest(value: &Value) -> String {
    sha256_hex("", &render(value))
}

/// The digest of one capability record.
#[must_use]
pub fn record_digest(record: &Value) -> String {
    sha256_hex(RECORD_DOMAIN, &render(record))
}

/// The digest of a definition version's content.
#[must_use]
pub fn definition_digest(content: &Value) -> String {
    sha256_hex(DEFINITION_DOMAIN, &render(content))
}

/// The digest of a mapping version's content.
#[must_use]
pub fn mapping_digest(content: &Value) -> String {
    sha256_hex(MAPPING_DOMAIN, &render(content))
}

/// The digest of a qualified profile version's content.
#[must_use]
pub fn profile_digest(content: &Value) -> String {
    sha256_hex(PROFILE_DOMAIN, &render(content))
}

/// Why a snapshot digest cannot be computed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DigestError {
    /// A record has no string `id` to sort by.
    ItemWithoutId,
    /// The observation time has no UTC instant or no RFC 3339 form (a year outside 0000 to
    /// 9999).
    InstantOutOfRange,
}

impl fmt::Display for DigestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ItemWithoutId => "a snapshot record has no string id",
            Self::InstantOutOfRange => "the observation time has no RFC 3339 form",
        })
    }
}

impl std::error::Error for DigestError {}

#[cfg(test)]
#[path = "digest_tests.rs"]
mod tests;
