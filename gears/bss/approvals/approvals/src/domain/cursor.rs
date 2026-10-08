//! The composite cursor: base64 JSON, one key per source, the order beside the narrowing hash.

use std::collections::BTreeMap;

use aws_lc_rs::digest::{SHA256, digest as sha256};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bss_approvals_sdk::{Order, SortKey, SourceNarrowing};
use serde::{Deserialize, Serialize};
use toolkit_canonical_errors::CanonicalError;
use toolkit_odata::Error as ODataError;
use uuid::Uuid;

const CURSOR_VERSION: u32 = 2;

/// A cursor the inbox minted, after it has been checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedCursor {
    /// The order the cursor was cut in.
    pub order: Order,
    /// Hash of the narrowing the cursor was cut under.
    pub narrowing_hash: String,
    /// Each source's last taken key. `None` means that source is still at the start.
    pub keys: BTreeMap<String, Option<SortKey>>,
    /// Sources that were down when this cursor was cut. A continuation does not ask them.
    pub unavailable: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredCursor {
    v: u32,
    order: Order,
    narrowing_hash: String,
    keys: BTreeMap<String, Option<SortKey>>,
    /// Absent on a token this version does not mint. Decode still requires version 2.
    #[serde(default)]
    unavailable: Vec<String>,
}

/// The four narrowing fields, in a fixed order, with absent values as JSON null.
#[derive(Serialize)]
struct NarrowingCanon<'a> {
    book_id: Option<Uuid>,
    kind: Option<&'a str>,
    ref_id: Option<Uuid>,
    state: Option<&'a str>,
}

/// The narrowing's identity. The order is not part of it.
#[must_use]
pub fn narrowing_hash(narrowing: &SourceNarrowing) -> String {
    let canonical = NarrowingCanon {
        book_id: narrowing.book_id,
        kind: narrowing.kind.as_deref(),
        ref_id: narrowing.ref_id,
        state: narrowing.state.as_deref(),
    };
    let bytes = serde_json::to_vec(&canonical).unwrap_or_default();
    hex_digest(sha256(&SHA256, &bytes).as_ref())
}

/// Encodes the cursor the next page sends back.
///
/// # Errors
/// The cursor value could not be encoded, which is an internal error.
pub fn encode(
    order: Order,
    narrowing_hash: &str,
    keys: &BTreeMap<String, Option<SortKey>>,
    unavailable: &[String],
) -> Result<String, CanonicalError> {
    let stored = StoredCursor {
        v: CURSOR_VERSION,
        order,
        narrowing_hash: narrowing_hash.to_owned(),
        keys: keys.clone(),
        unavailable: unavailable.to_vec(),
    };
    let raw = serde_json::to_vec(&stored).map_err(|err| {
        CanonicalError::internal(format!("bss-approvals: cursor did not encode: {err}")).create()
    })?;
    Ok(URL_SAFE_NO_PAD.encode(raw))
}

/// Decodes a cursor token.
///
/// # Errors
/// 400 `INVALID_CURSOR` when the token is not this version's JSON.
pub fn decode(token: &str) -> Result<DecodedCursor, CanonicalError> {
    let raw = URL_SAFE_NO_PAD
        .decode(token)
        .map_err(|_| CanonicalError::from(ODataError::CursorInvalidBase64))?;
    let stored: StoredCursor = serde_json::from_slice(&raw)
        .map_err(|_| CanonicalError::from(ODataError::CursorInvalidJson))?;
    if stored.v != CURSOR_VERSION {
        return Err(ODataError::CursorInvalidVersion.into());
    }
    Ok(DecodedCursor {
        order: stored.order,
        narrowing_hash: stored.narrowing_hash,
        keys: stored.keys,
        unavailable: stored.unavailable,
    })
}

fn hex_digest(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut hex = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        hex.push(char::from(DIGITS[usize::from(byte >> 4)]));
        hex.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    hex
}
