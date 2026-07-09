//! HTTP `Range` header parsing into the [`ByteRange`] domain type.
//!
//! Only single-range `bytes=` requests are supported (the common media-scrubbing
//! case); multi-range requests are rejected so the caller can fall back to a
//! full-body response.

use file_storage_sdk::ByteRange;

/// Parse a single-range `Range` header value (e.g. `bytes=0-1023`, `bytes=512-`,
/// `bytes=-256`). Returns `None` for absent/unsupported/malformed values.
#[must_use]
pub fn parse(header: &str) -> Option<ByteRange> {
    let spec = header.trim().strip_prefix("bytes=")?;
    // Multi-range ("a-b,c-d") is not supported.
    if spec.contains(',') {
        return None;
    }
    let (start, end) = spec.split_once('-')?;
    let start = start.trim();
    let end = end.trim();

    match (start.is_empty(), end.is_empty()) {
        // "-N" → suffix
        (true, false) => end
            .parse::<u64>()
            .ok()
            .map(|length| ByteRange::Suffix { length }),
        // "N-" → open-ended
        (false, true) => start
            .parse::<u64>()
            .ok()
            .map(|start| ByteRange::OpenEnded { start }),
        // "N-M" → inclusive
        (false, false) => {
            let s = start.parse::<u64>().ok()?;
            let e = end.parse::<u64>().ok()?;
            Some(ByteRange::Inclusive { start: s, end: e })
        }
        // "-" → malformed
        (true, true) => None,
    }
}

#[cfg(test)]
#[path = "range_tests.rs"]
mod range_tests;
