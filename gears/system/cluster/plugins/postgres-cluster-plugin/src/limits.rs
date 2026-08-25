//! The length ceiling shared by every value this plugin stores in an indexed
//! `TEXT` primary key (DESIGN.md §2.1).
//!
//! Both owned tables key on an unbounded `TEXT` column — `cluster_cache.key`
//! and `cluster_lock.name` — and both are `PRIMARY KEY`, so the value lands in
//! a btree. That is the binding constraint, and it is *not* the same as the
//! NOTIFY payload budget the two backends previously bounded themselves by:
//! `MAX_KEY_BYTES` (7997) and `MAX_LOCK_NAME_BYTES` (7999) were both derived
//! from `MAX_NOTIFY_PAYLOAD_LENGTH` and both sit well *above* the btree limit,
//! leaving a window in which a key passed the plugin's own guard and then
//! failed mid-write inside Postgres.
//!
//! Storage width is not the issue: Postgres stores `TEXT` and `VARCHAR(n)`
//! identically on disk, and `VARCHAR(n)` is just `TEXT` plus a length check.
//! TOAST does not help either — an indexed key is not `TOAST`ed out of the
//! index tuple, so the tuple has to fit as-is.

/// `PostgreSQL`'s hard ceiling on a single btree index tuple: roughly one third
/// of an 8 KB page, reported by the server as `index row size N exceeds btree
/// version 4 maximum 2704 for index "..."` (SQLSTATE `54000`,
/// `program_limit_exceeded`). An insert past this fails outright — there is no
/// degraded mode to fall back to.
pub const BTREE_MAX_INDEX_TUPLE_BYTES: usize = 2704;

/// The longest value, in UTF-8 bytes, this plugin will accept for an indexed
/// key or lock name.
///
/// Deliberately below [`BTREE_MAX_INDEX_TUPLE_BYTES`] rather than equal to it:
/// the quoted 2704 is the budget for the whole index tuple, not just the key
/// bytes, so per-tuple header overhead has to fit alongside. The headroom also
/// leaves room for the reaper's and `scan_prefix`'s composite lookups without
/// re-deriving the arithmetic per call site.
///
/// This is generous against what the SDK can legitimately produce: a consumer
/// key and each scope prefix are independently capped at 255 bytes
/// (`cluster_sdk::scope::MAX_SCOPE_PREFIX_LEN`), so 2048 accommodates a leaf key
/// under roughly seven levels of maximum-length scope nesting. Note that the SDK
/// caps each prefix but *not* their composition, so nesting depth is unbounded.
pub const MAX_INDEXED_KEY_BYTES: usize = 2048;

// The point of the constant is the margin; assert it rather than trusting the
// two literals to stay in the right order through a later edit.
const _: () = assert!(MAX_INDEXED_KEY_BYTES < BTREE_MAX_INDEX_TUPLE_BYTES);
