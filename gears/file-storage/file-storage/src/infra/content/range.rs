//! HTTP `Range` header parsing into the [`ByteRange`] domain type.
//!
//! Only single-range `bytes=` requests are supported (the common media-scrubbing
//! case); multi-range requests are rejected so the caller can fall back to a
//! full-body response.

use file_storage_sdk::ByteRange;

/// Parse a single-range `Range` header value (e.g. `bytes=0-1023`, `bytes=512-`,
/// `bytes=-256`). Returns `None` for absent/unsupported/malformed values, per
/// RFC 9110 §14.1.1 a header that fails to parse as a valid `ranges-specifier`
/// MUST be ignored by the recipient (i.e. treated as if absent, serving a
/// full-body `200` rather than a `416`) — the caller is expected to do so.
#[must_use]
pub fn parse(header: &str) -> Option<ByteRange> {
    let spec = header.trim().strip_prefix("bytes=")?;
    // Multi-range ("a-b,c-d") is not supported. RFC 9110 allows OWS around
    // each comma in a real multi-range list, but since this module doesn't
    // parse multi-range at all (any comma just falls back to a full-body
    // response), there's nothing further to normalize here.
    if spec.contains(',') {
        return None;
    }
    let (start, end) = spec.split_once('-')?;

    match (start.is_empty(), end.is_empty()) {
        // "-N" → suffix
        (true, false) => parse_digits(end).map(|length| ByteRange::Suffix { length }),
        // "N-" → open-ended
        (false, true) => parse_digits(start).map(|start| ByteRange::OpenEnded { start }),
        // "N-M" → inclusive.
        (false, false) => {
            let s = parse_digits(start)?;
            let e = parse_digits(end)?;
            // RFC 9110 §14.1.1: a byte-range-spec whose last-byte-pos is less
            // than first-byte-pos is invalid syntax (not "unsatisfiable") and
            // MUST be ignored. `end == start` (e.g. `bytes=0-0`, a single
            // byte) is still valid.
            if e < s {
                return None;
            }
            Some(ByteRange::Inclusive { start: s, end: e })
        }
        // "-" → malformed
        (true, true) => None,
    }
}

/// Strict `1*DIGIT` per RFC 9110's ABNF: ASCII digits only — no sign (`+`/`-`),
/// no interior whitespace, no other numeric syntax that `u64::from_str` might
/// otherwise be lenient about.
fn parse_digits(s: &str) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<u64>().ok()
}

#[cfg(test)]
#[path = "range_tests.rs"]
mod range_tests;
