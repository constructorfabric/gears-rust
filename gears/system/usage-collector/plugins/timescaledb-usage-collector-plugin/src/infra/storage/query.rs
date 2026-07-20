//! Injection-safe `OData` → SQL translation foundation.
//!
//! Pure (no DB) logic that turns a validated `toolkit_odata` filter AST into a
//! parameterized `PostgreSQL` `WHERE` fragment plus an ordered list of binds.
//! Every SQL identifier is drawn from a closed allowlist
//! ([`translate::record_column`] / [`translate::usage_type_column`]); every
//! value is bound (`$N`), never interpolated.

pub mod aggregate;
pub mod bind;
pub mod keyset;
pub mod translate;

/// Hard upper bound on the page size either list path will request from
/// `PostgreSQL` in a single `fetch_all`, regardless of the caller's `$top`.
///
/// This is a **defense-in-depth backstop**, not the primary cap: the
/// usage-collector core gateway already rejects `$top > 1000` with
/// `400 InvalidArgument` (its own `MAX_PAGE_SIZE`) before any plugin call.
/// The value is kept in lock-step with that gateway cap so this clamp is
/// never reached in normal operation — it only bites if the plugin is ever
/// driven by a different or buggy caller, preventing an unbounded
/// full-result-set read (a resource/DoS hazard) at the persistence boundary.
pub const MAX_PAGE_SIZE: u64 = 1000;

/// Resolve the effective `LIMIT` for a list query: the caller's `$top`
/// (`requested`) when present, else `default_page_size`, clamped to the
/// `[1, MAX_PAGE_SIZE]` range.
///
/// Clamping (rather than rejecting) is correct here because the plugin is
/// pure persistence and must not own HTTP-policy `4xx`s — the reject already
/// lives in the core. Keyset pagination degrades gracefully under a clamp:
/// the look-ahead + `next_cursor` still yield a correct, resumable page, just
/// a smaller one than an out-of-contract caller asked for.
///
/// The **lower** bound of 1 is not cosmetic: `$top=0` is a legal `OData` value
/// the core gateway forwards unclamped, and a resolved page size of 0 would
/// drive `LIMIT 0+1 = 1` then `truncate(0)` on the look-ahead read — leaving
/// `rows.last()` `None` on a non-empty table and 500-ing both list paths at
/// `encode_next_cursor`. Flooring to the smallest legal page (1) keeps the
/// look-ahead invariant intact without the plugin minting a `4xx`.
#[must_use]
pub fn effective_page_size(requested: Option<u64>, default_page_size: u64) -> u64 {
    requested
        .unwrap_or(default_page_size)
        .clamp(1, MAX_PAGE_SIZE)
}

#[cfg(test)]
#[path = "query_tests.rs"]
mod query_tests;
