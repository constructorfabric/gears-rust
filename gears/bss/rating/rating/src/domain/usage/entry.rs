//! A feed entry as captured: its identity and its versioned digest (DESIGN [12 §4.1], §4.3).
//!
//! `raw_sha256` digests a **pinned, versioned field list**, never the whole entry: the collector
//! may add optional fields within a major version, and a whole-entry digest would change on every
//! re-read after such an addition (V74). A re-read entry is digested under the stored row's
//! version and compared only with it.
//!
//! [12 §4.1]: ../../../../docs/DESIGN.md#contract-12-4-1

use serde_json::{Map, Value};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::domain::digest::Sha256Hex;

/// A `raw_digest_version`: which pinned field list a stored digest covers. A stored version this
/// build does not know is kept out of the type at the storage boundary.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DigestVersion {
    /// [`DIGEST_V1_FIELDS`].
    V1,
}

impl DigestVersion {
    /// The stored number.
    #[must_use]
    pub const fn as_i16(self) -> i16 {
        match self {
            Self::V1 => 1,
        }
    }

    /// The version a stored number denotes, or `None` for one this build does not know.
    #[must_use]
    pub const fn parse(version: i16) -> Option<Self> {
        match version {
            1 => Some(Self::V1),
            _ => None,
        }
    }
}

/// The digest version new captures are written under.
pub const CURRENT_DIGEST_VERSION: DigestVersion = DigestVersion::V1;

/// The largest entry, as JSON text, a row stores whole (`raw_entry`). The collector bounds every
/// field but `metadata` values; a larger entry is a contract breach, stored as a bounded stub.
pub const MAX_RAW_ENTRY_BYTES: usize = 64 * 1024;

/// How much of an oversized entry's JSON text its stub keeps.
const OVERSIZE_EXCERPT_BYTES: usize = 1024;

/// The fields `raw_digest_version = 1` digests: the usage-collector v2 record's caller-supplied
/// fields and its server-assigned `id`, `accepted_at`, `origin` and `invalidates`. `entry_type` is
/// not one of them: the SDK record does not carry it, and it adds nothing to `invalidates`.
pub const DIGEST_V1_FIELDS: [&str; 14] = [
    "accepted_at",
    "gts_type_id",
    "id",
    "idempotency_key",
    "invalidates",
    "metadata",
    "origin",
    "quantity",
    "reason_code",
    "resource_ref",
    "subject_ref",
    "tenant_id",
    "window_end",
    "window_start",
];

/// The instants of the v1 field list, digested in the collector's identity form.
const DIGEST_V1_INSTANTS: [&str; 3] = ["accepted_at", "window_end", "window_start"];

/// What an entry is stored under: its tenant and the collector's id.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EntryIdentity {
    /// `tenant_id`: the resource tenant that owns the row.
    pub tenant_id: Uuid,
    /// `id`: the collector's entry id.
    pub usage_record_id: Uuid,
}

/// The identity of `entry`, or `None` when its `id` or `tenant_id` cannot be read: such an entry
/// is a capture reject and opens a source loss (feature 04 §3.1).
#[must_use]
pub fn identify(entry: &Value) -> Option<EntryIdentity> {
    let uuid = |field: &str| {
        entry
            .get(field)
            .and_then(Value::as_str)
            .and_then(|text| Uuid::parse_str(text).ok())
            .filter(|id| !id.is_nil())
    };
    Some(EntryIdentity {
        tenant_id: uuid("tenant_id")?,
        usage_record_id: uuid("id")?,
    })
}

/// The digest of `entry` under `version`.
#[must_use]
pub fn digest(entry: &Value, version: DigestVersion) -> Sha256Hex {
    match version {
        DigestVersion::V1 => digest_v1(entry),
    }
}

/// An entry as a row stores it: whole, or a stub when its JSON text is larger than
/// [`MAX_RAW_ENTRY_BYTES`].
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedEntry {
    /// What `raw_entry` holds.
    pub value: Value,
    /// The entry's size in bytes when it was cut to a stub.
    pub oversize: Option<usize>,
}

/// `entry` bounded for storage. The stub `{"oversize": {"bytes", "excerpt"}}` keeps the first
/// kilobyte of the JSON text, enough for an operator to see what the collector sent; the digests
/// are still taken over the whole entry, so a re-read is recognized.
#[must_use]
pub fn bounded(entry: &Value) -> BoundedEntry {
    let text = entry.to_string();
    if text.len() <= MAX_RAW_ENTRY_BYTES {
        return BoundedEntry {
            value: entry.clone(),
            oversize: None,
        };
    }
    let mut cut = OVERSIZE_EXCERPT_BYTES;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    BoundedEntry {
        value: serde_json::json!({
            "oversize": {"bytes": text.len(), "excerpt": &text[..cut]},
        }),
        oversize: Some(text.len()),
    }
}

/// The digest of a capture reject, keyed `(source_id, raw_sha256)`: the whole entry, since its
/// fields cannot be trusted.
#[must_use]
pub fn reject_digest(entry: &Value) -> Sha256Hex {
    Sha256Hex::of_json(entry)
}

fn digest_v1(entry: &Value) -> Sha256Hex {
    let mut pinned = Map::new();
    for field in DIGEST_V1_FIELDS {
        let value = entry.get(field).cloned();
        let value = match (field, value) {
            // The SDK omits an empty `metadata`, so absent and `{}` are one value.
            ("metadata", None | Some(Value::Null)) => Value::Object(Map::new()),
            (field, Some(Value::String(text))) if DIGEST_V1_INSTANTS.contains(&field) => {
                Value::String(canonical_instant(&text).unwrap_or(text))
            }
            (_, value) => value.unwrap_or(Value::Null),
        };
        pinned.insert(field.to_owned(), value);
    }
    Sha256Hex::of_json(&Value::Object(pinned))
}

/// `text` in the collector's identity form `YYYY-MM-DDTHH:MM:SS.ffffffZ`, or `None` when it is no
/// RFC 3339 instant that form holds exactly (a sub-microsecond part, a year beyond four digits).
#[must_use]
pub fn canonical_instant(text: &str) -> Option<String> {
    let instant = OffsetDateTime::parse(text, &Rfc3339).ok()?;
    let utc = instant.to_offset(time::UtcOffset::UTC);
    if !utc.nanosecond().is_multiple_of(1_000) || !(0..=9999).contains(&utc.year()) {
        return None;
    }
    Some(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:06}Z",
        utc.year(),
        u8::from(utc.month()),
        utc.day(),
        utc.hour(),
        utc.minute(),
        utc.second(),
        utc.microsecond()
    ))
}

#[cfg(test)]
#[path = "entry_tests.rs"]
mod tests;
