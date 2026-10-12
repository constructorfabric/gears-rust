//! HTTP `Range` header parsing into the [`ByteRange`] domain type.
//!
//! Only single-range `bytes=` requests are supported; multi-range requests are rejected so
//! the caller can fall back to a full-body response.

use file_storage_sdk::ByteRange;

/// Parse a single-range `Range` value (`bytes=0-1023`, `bytes=512-`, `bytes=-256`).
/// `None` for unsupported/malformed values, which per RFC 9110 §14.1.1 must be ignored
/// (full-body `200`, not `416`).
#[must_use]
pub fn parse(header: &str) -> Option<ByteRange> {
    let spec = header.trim().strip_prefix("bytes=")?;
    // Multi-range ("a-b,c-d") is unsupported: any comma falls back to a full body.
    if spec.contains(',') {
        return None;
    }
    let (start, end) = spec.split_once('-')?;

    match (start.is_empty(), end.is_empty()) {
        (true, false) => parse_digits(end).map(|length| ByteRange::Suffix { length }),
        (false, true) => parse_digits(start).map(|start| ByteRange::OpenEnded { start }),
        (false, false) => {
            let s = parse_digits(start)?;
            let e = parse_digits(end)?;
            // last < first is invalid syntax (not "unsatisfiable") and is ignored;
            // `end == start` is a valid single byte.
            if e < s {
                return None;
            }
            Some(ByteRange::Inclusive { start: s, end: e })
        }
        (true, true) => None,
    }
}

/// Strict `1*DIGIT` (RFC 9110): ASCII digits only, no sign or whitespace that
/// `u64::from_str` might accept.
fn parse_digits(s: &str) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<u64>().ok()
}

#[cfg(test)]
#[path = "range_tests.rs"]
mod range_tests;
