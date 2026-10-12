//! Fingerprints of proposed business content and the common effective date.

use crate::model::ItemRef;
use aws_lc_rs::digest::{SHA256, digest};
use time::Date;

/// SHA-256 hex of canonical JSON (sorted keys, items sorted by type then id) of what
/// the reviewer approves: every item's `after` and the common effective date.
#[must_use]
pub fn snapshot_hash(items: &[ItemRef], common_effective_date: Option<Date>) -> String {
    let mut sorted: Vec<&ItemRef> = items.iter().collect();
    sorted.sort_by(|a, b| (&a.item_type, a.item_id).cmp(&(&b.item_type, b.item_id)));
    let canon = serde_json::json!({
        "commonEffectiveDate": common_effective_date.map(|d| d.to_string()),
        "items": sorted.iter().map(|i| serde_json::json!({ "type": i.item_type, "id": i.item_id, "after": canonical(&i.after) })).collect::<Vec<_>>(),
    });
    // A `Value` always serialises; if it ever did not, the fingerprint is the
    // digest of the error text, which can equal nothing a healthy run produces.
    let bytes =
        serde_json::to_vec(&canonical(&canon)).unwrap_or_else(|e| e.to_string().into_bytes());
    let digested = digest(&SHA256, &bytes);
    hex_encode(digested.as_ref())
}

fn canonical(v: &serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            serde_json::Value::Object(
                keys.into_iter()
                    .map(|k| (k.clone(), canonical(&m[k])))
                    .collect(),
            )
        }
        serde_json::Value::Array(a) => serde_json::Value::Array(a.iter().map(canonical).collect()),
        other => other.clone(),
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut hex = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        hex.push(char::from(DIGITS[usize::from(byte >> 4)]));
        hex.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    hex
}

#[cfg(test)]
#[path = "hash_tests.rs"]
mod hash_tests;
