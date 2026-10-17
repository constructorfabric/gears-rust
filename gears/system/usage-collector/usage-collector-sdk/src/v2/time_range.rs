//! The read-path covered-period range.
//!
//! Both range-taking read paths — the keyset-paginated raw list and the
//! aggregate (the point lookup selects by `id` and takes no range) — take
//! a mandatory bounded range, and one rule says which
//! entries it selects: `from <= window_end < to`
//! (`cpt-cf-usage-collector-adr-window-end-selection`). The rule reads the
//! period **end** and nothing else, so it needs no case for a point event
//! (`window_start == window_end`), it partitions the entries so adjacent
//! ranges sum without double counting, and it lets a plugin serve a range
//! from a rollup keyed on one column.
//!
//! The type travels as a typed parameter on the SDK trait and the Plugin
//! SPI, and as a first-class parameter on the REST surface — `from` / `to`
//! query parameters on the raw path, `AggregationRequest.time_range` in the
//! aggregate body. It is never a `$filter` conjunct, which is why the
//! covered-period fields are reserved on the filter surface.

use time::{OffsetDateTime, UtcOffset};

use crate::v2::error::UsageCollectorError;

/// A validated, UTC-normalized read range: `[from, to)`.
///
/// Constructed through [`TimeRange::new`], which is the only way to obtain
/// one — the fields are private, so an unordered range cannot reach a
/// storage plugin. `Copy`, so it passes by value on every surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeRange {
    from: OffsetDateTime,
    to: OffsetDateTime,
}

impl TimeRange {
    /// Creates a range after normalizing both bounds to UTC and checking
    /// that it is strictly ordered.
    ///
    /// Normalization moves the offset, never the instant, so a caller that
    /// sends `13:00:00+01:00` and one that sends `12:00:00Z` obtain the same
    /// range — the same equivalence the covered period gets on ingestion.
    ///
    /// # Errors
    ///
    /// Returns [`UsageCollectorError::InvalidArgument`] when `to <= from`.
    pub fn new(from: OffsetDateTime, to: OffsetDateTime) -> Result<Self, UsageCollectorError> {
        let from = from.to_offset(UtcOffset::UTC);
        let to = to.to_offset(UtcOffset::UTC);
        if to <= from {
            return Err(UsageCollectorError::invalid_time_range(from, to));
        }
        Ok(Self { from, to })
    }

    /// The inclusive lower bound, in UTC.
    #[must_use]
    pub fn lower_inclusive(self) -> OffsetDateTime {
        self.from
    }

    /// The exclusive upper bound, in UTC.
    #[must_use]
    pub fn upper_exclusive(self) -> OffsetDateTime {
        self.to
    }

    /// The range's canonical text form, `<from>~<to>`, each bound rendered
    /// as its nanoseconds since the Unix epoch.
    ///
    /// One spelling, because this value ends up inside an opaque pagination
    /// cursor: the gear folds it into the fingerprint a keyset continuation
    /// is bound to, and the caller's next request has to recompute an
    /// identical string or every cursor minted under the first spelling is
    /// refused. Nanoseconds since the epoch make that reproducible — the
    /// rendering is instant-based, so it is offset-invariant as well as
    /// lossless, and two spellings of one instant render identically.
    #[must_use]
    pub fn canonical_form(self) -> String {
        // Full nanosecond resolution, deliberately NOT
        // `crate::v2::id::canonical_period_bound`'s fixed-microsecond form,
        // which refuses a finer bound: composing the two would collapse two ranges
        // differing only below the microsecond onto one fingerprint, so a
        // cursor minted under either would validate against the other.
        format!(
            "{}~{}",
            self.from.unix_timestamp_nanos(),
            self.to.unix_timestamp_nanos(),
        )
    }

    /// Does this range select an entry whose covered period ends at
    /// `window_end`?
    ///
    /// The reference spelling of `from <= window_end < to`: every
    /// in-process implementation calls this rather than re-deriving it, and a
    /// SQL-backed plugin restates the same predicate in its own `WHERE`
    /// clause (it cannot call a Rust method).
    ///
    /// Comparison is instant-based: [`OffsetDateTime`]'s `Ord` normalizes
    /// its operand into the receiver's offset first, so `window_end` in any
    /// offset compares correctly without an explicit conversion here.
    #[must_use]
    pub fn contains_window_end(self, window_end: OffsetDateTime) -> bool {
        self.from <= window_end && window_end < self.to
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "time_range_tests.rs"]
mod time_range_tests;
