//! Shared cursor contract for the raw-query endpoint.
//!
//! Both the gateway (when validating a caller-supplied cursor) and any
//! storage plugin (when minting `cursor.s` / `cursor.f`) must agree on:
//!
//! - the effective sort signature (`+timestamp,+id`, ascending), and
//! - the canonical rendering + SHA-256 hash of the request's filter set.
//!
//! Keeping both in this crate (alongside [`crate::RawQuery`] and the
//! re-exported [`modkit_odata::CursorV1`]) ensures the gateway and any
//! out-of-process plugin link the same byte-for-byte implementation; a
//! mismatch would silently invalidate every cursor the other side mints.
//!
//! The canonical-string format is part of the wire contract — see
//! [`raw_query_filter_hash`] for the format and the pin tests that lock it
//! in.

use std::sync::LazyLock;

use chrono::{DateTime, SecondsFormat, Utc};
use modkit_odata::{ODataOrderBy, OrderKey, SortDir};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::RawQuery;

/// Filter inputs that participate in the raw-query cursor's filter hash
/// (`cursor.f`). A borrowed view: the gateway builds one from its REST DTO
/// before PDP, a plugin builds one from [`RawQuery`] post-PDP, and both
/// pass it through [`raw_query_filter_hash`] to compute the same hex
/// digest.
///
/// Using named fields (rather than positional arguments to a hash function)
/// makes it impossible to silently swap `resource_type` ↔ `subject_type` or
/// `resource_id` ↔ `subject_id` at a call site — a swap that would yield a
/// hash disagreeing with any plugin computing it correctly.
#[derive(Debug, Clone, Copy)]
pub struct RawQueryFilters<'a> {
    /// Inclusive lower bound of the query's time range.
    pub from: DateTime<Utc>,
    /// Inclusive upper bound of the query's time range.
    pub to: DateTime<Utc>,
    /// Optional usage type filter.
    pub usage_type: Option<&'a str>,
    /// Optional resource UUID filter.
    pub resource_id: Option<Uuid>,
    /// Optional resource type filter.
    pub resource_type: Option<&'a str>,
    /// Optional subject type filter.
    pub subject_type: Option<&'a str>,
    /// Optional subject UUID filter.
    pub subject_id: Option<Uuid>,
}

impl<'a> From<&'a RawQuery> for RawQueryFilters<'a> {
    fn from(q: &'a RawQuery) -> Self {
        Self {
            from: q.time_range.0,
            to: q.time_range.1,
            usage_type: q.usage_type.as_deref(),
            resource_id: q.resource_id,
            resource_type: q.resource_type.as_deref(),
            subject_type: q.subject_type.as_deref(),
            subject_id: q.subject_id,
        }
    }
}

/// Lazily-initialized backing storage for [`raw_query_effective_order`]. The
/// effective order is a request-path constant — every raw-query call with a
/// cursor re-uses the same `+timestamp,+id` signature — so we materialize the
/// `Vec<OrderKey>` (and its two heap `String`s) once per process rather than
/// per request.
static RAW_QUERY_EFFECTIVE_ORDER: LazyLock<ODataOrderBy> = LazyLock::new(|| {
    ODataOrderBy(vec![
        OrderKey {
            field: "timestamp".to_owned(),
            dir: SortDir::Asc,
        },
        OrderKey {
            field: "id".to_owned(),
            dir: SortDir::Asc,
        },
    ])
});

/// Effective sort signature the raw endpoint always issues cursors against:
/// ascending forward keyset on `(timestamp, id)`. Centralized so the gateway
/// and any storage plugin minting cursors agree on the same `cursor.s`
/// shape.
///
/// Returns a `'static` reference into a process-wide `LazyLock` so callers on
/// the request path do not allocate a fresh `Vec<OrderKey>` (with two heap
/// `String`s) on every cursor decode.
pub fn raw_query_effective_order() -> &'static ODataOrderBy {
    &RAW_QUERY_EFFECTIVE_ORDER
}

/// Compute a stable 16-char hex hash of the raw query's effective filter set
/// (time range plus the structured `usage_type` / `resource_id` /
/// `resource_type` / `subject_type` / `subject_id` filters). The raw endpoint
/// does not use `OData` `$filter` expressions, so `short_filter_hash` (which
/// hashes an `ast::Expr`) is not applicable; we hash a canonical pipe-
/// separated rendering of the structured fields instead.
///
/// # Wire contract
///
/// The canonical string format is **stable across builds and across gateway
/// instances**. Any future storage plugin that mints `cursor.f` MUST agree
/// byte-for-byte on this rendering — otherwise the gateway's filter-hash
/// check (see `modkit_odata::validate_cursor_against`) will reject every
/// plugin-minted cursor. The format is pinned by `pin_filter_hash_*` tests in
/// this module; changing it requires updating those tests and is a wire
/// break that obsoletes every previously minted cursor.
///
/// Format: `TIME({from},{to})|UT({ut})|RID({rid})|RT({rt})|ST({st})|SID({sid})`
/// where `{from}`/`{to}` are RFC 3339 with nanosecond precision and `Z`
/// suffix, UUIDs are hyphenated lowercase, and each optional field renders as
/// the literal token `None` when absent or `Some({value})` when present. The
/// `None`/`Some(...)` distinction is intentional: rendering absent and
/// empty-string fields identically (e.g. both as `""`) would collapse a
/// `?usage_type=` (which axum deserializes as `Some("")`) and an omitted
/// `?usage_type` into the same digest, defeating the filter-hash gate. Pin
/// tests in `cursor_filter_tests.rs` exercise the empty-string-vs-`None`
/// divergence explicitly.
#[must_use]
pub fn raw_query_filter_hash(filters: &RawQueryFilters<'_>) -> String {
    let canonical = raw_query_filter_canonical(filters);
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    hex::encode(&hasher.finalize()[..8])
}

/// Render a [`RawQueryFilters`] as the canonical pipe-separated string that
/// feeds [`raw_query_filter_hash`]. Split out of the hash function so the
/// pin tests can assert the byte-for-byte string contract (the digest is a
/// derivative — pinning the digest alone lets a coordinated drift in the
/// format + constant slip through review).
///
/// `pub(crate)` rather than `pub` to keep the wire contract reachable from the
/// in-crate pin tests without exposing a second public symbol that a plugin
/// might depend on; [`raw_query_filter_hash`] remains the only public entry
/// point.
pub(crate) fn raw_query_filter_canonical(filters: &RawQueryFilters<'_>) -> String {
    format!(
        "TIME({from},{to})|UT({ut})|RID({rid})|RT({rt})|ST({st})|SID({sid})",
        from = filters.from.to_rfc3339_opts(SecondsFormat::Nanos, true),
        to = filters.to.to_rfc3339_opts(SecondsFormat::Nanos, true),
        ut = render_opt_str(filters.usage_type),
        rid = render_opt_uuid(filters.resource_id),
        rt = render_opt_str(filters.resource_type),
        st = render_opt_str(filters.subject_type),
        sid = render_opt_uuid(filters.subject_id),
    )
}

/// Render an optional string filter as `None` (absent) or `Some({value})`
/// (present). The marker prefix is what prevents an empty-string value from
/// hashing identically to an absent field — see [`raw_query_filter_hash`].
fn render_opt_str(v: Option<&str>) -> String {
    match v {
        None => "None".to_owned(),
        Some(s) => format!("Some({s})"),
    }
}

/// UUID counterpart of [`render_opt_str`]. UUIDs serialize as hyphenated
/// lowercase (the `Display`/`Hyphenated` impl) and never produce an empty
/// string, but the `None`/`Some(...)` wrapper is still applied for symmetry
/// so the canonical format is the same shape for every optional field.
fn render_opt_uuid(v: Option<Uuid>) -> String {
    match v {
        None => "None".to_owned(),
        Some(u) => format!("Some({})", u.as_hyphenated()),
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "cursor_filter_tests.rs"]
mod cursor_filter_tests;
