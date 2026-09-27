//! Cursor codec shared by the gear's three SQL-backed keyset listings
//! (`GET /files`, `GET /files/{id}/versions`, `GET /retention-rules`, per
//! `guidelines/DNA/REST/QUERYING.md`).
//!
//! All three share one page shape: `created_at DESC` then an id-column
//! tie-breaker, also `DESC` (newest first -- the same order the previous
//! offset pagination already used), forward-only. Building/parsing the
//! opaque token itself lives here so the REST handlers, the domain services,
//! and the SDK's in-process local client all share one codec instead of each
//! re-deriving the `CursorV1` shape; the repo layer only ever sees a decoded
//! [`Seek`] (typed `created_at`/id values), never the wire token.
//!
//! No `prev_cursor` is ever built (always `None` on the wire) and a
//! client-supplied `"bwd"` direction is rejected outright -- this platform's
//! listings are forward-only (no speculative backward paging).

use time::OffsetDateTime;
use uuid::Uuid;

use toolkit_db::odata::sea_orm_filter::{encode_cursor_value, parse_cursor_value};
use toolkit_odata::filter::FieldKind;
use toolkit_odata::{CursorV1, Error as ODataError, ODataOrderBy, OrderKey, SortDir};

use file_storage_sdk::OwnerFilter;

/// `GET /files`'s id tie-breaker column.
pub const FILES_ID_FIELD: &str = "file_id";
/// `GET /files/{id}/versions`'s id tie-breaker column.
pub const VERSIONS_ID_FIELD: &str = "version_id";
/// `GET /retention-rules`'s id tie-breaker column.
pub const RETENTION_RULES_ID_FIELD: &str = "rule_id";

/// This platform's listings never build a backward page.
const FORWARD: &str = "fwd";

/// The one keyset shape every listing here uses: `created_at` then the
/// listing's own id column, both descending.
fn canonical_order(id_field: &'static str) -> ODataOrderBy {
    ODataOrderBy(vec![
        OrderKey {
            field: "created_at".to_owned(),
            dir: SortDir::Desc,
        },
        OrderKey {
            field: id_field.to_owned(),
            dir: SortDir::Desc,
        },
    ])
}

/// A stable fingerprint for whatever this cursor is bound to (an owner pair
/// for `/files`, a `file_id` for `/files/{id}/versions`; `/retention-rules`
/// has none). Reuses `toolkit_odata`'s own FNV hash (the same one filter
/// cursors elsewhere on the platform use for `f`) rather than
/// `std::hash::DefaultHasher`, which is explicitly not stable across process
/// runs and would reject every cursor issued by a previous process.
fn binding_hash(raw: &str) -> Option<String> {
    let expr = toolkit_odata::ast::Expr::Value(toolkit_odata::ast::Value::String(raw.to_owned()));
    toolkit_odata::pagination::short_filter_hash(Some(&expr))
}

/// `/files`' binding: a cursor issued for one owner pair cannot resume a
/// listing of a different owner pair.
#[must_use]
pub fn files_binding(owner: &OwnerFilter) -> Option<String> {
    binding_hash(&format!("{}:{}", owner.owner_kind.as_str(), owner.owner_id))
}

/// `/files/{id}/versions`' binding: a cursor issued for one file cannot
/// resume a listing of another file.
#[must_use]
pub fn versions_binding(file_id: Uuid) -> Option<String> {
    binding_hash(&file_id.to_string())
}

/// The decoded, already-validated position of the last row a previous page
/// returned -- everything a repo's keyset predicate needs to seek past it.
#[derive(Debug, Clone, Copy)]
pub struct Seek {
    pub created_at: OffsetDateTime,
    pub id: Uuid,
}

/// Decode and validate a cursor token against one listing's canonical order
/// and binding.
///
/// # Errors
/// `ODataError::InvalidCursor`/`CursorInvalid*` for an unreadable token or a
/// `"bwd"` direction (rejected outright -- see the module doc); `OrderMismatch`
/// for a token whose `s` doesn't match `id_field`'s canonical order;
/// `FilterMismatch` for a token bound to a different owner/file (or a bound
/// token replayed against an unbound listing, or vice versa).
pub fn decode(
    token: &str,
    id_field: &'static str,
    binding: Option<&str>,
) -> Result<Seek, ODataError> {
    let cursor = CursorV1::decode(token)?;
    if cursor.d != FORWARD {
        return Err(ODataError::InvalidCursor);
    }
    let order = canonical_order(id_field);
    if cursor.f.is_some() != binding.is_some() {
        return Err(ODataError::FilterMismatch);
    }
    toolkit_odata::validate_cursor_against(&cursor, &order, binding)?;
    let [created_at_s, id_s] = <[String; 2]>::try_from(cursor.k).map_err(|k| {
        tracing::debug!(
            key_count = k.len(),
            "file-storage: cursor key count mismatch"
        );
        ODataError::CursorInvalidKeys
    })?;
    let value = parse_cursor_value(FieldKind::DateTimeUtc, &created_at_s)
        .map_err(|_| ODataError::CursorInvalidFields)?;
    let sea_orm::Value::TimeDateTimeWithTimeZone(Some(created_at)) = value else {
        return Err(ODataError::CursorInvalidFields);
    };
    let id = id_s
        .parse::<Uuid>()
        .map_err(|_| ODataError::CursorInvalidFields)?;
    Ok(Seek { created_at, id })
}

/// Resolve a caller-supplied `?limit` against this listing's configured
/// `(default_page_size, max_page_size)`: `None` falls back to `default`, and
/// any value above `max` is silently clamped down to `max`
/// (`guidelines/DNA/REST/QUERYING.md`'s documented `limit.min(max)`
/// semantics). `Some(0)` is refused outright rather than clamped -- a caller
/// asking for zero rows is almost certainly a mistake, not a valid page size.
///
/// # Errors
/// `ODataError::InvalidLimit` for `limit == Some(0)`.
pub fn clamp_limit(limit: Option<u64>, default: u64, max: u64) -> Result<u64, ODataError> {
    match limit {
        Some(0) => Err(ODataError::InvalidLimit),
        Some(requested) => Ok(requested.min(max)),
        None => Ok(default.min(max)),
    }
}

/// Encode the position the next page should resume after (the last row this
/// page actually returned).
///
/// # Errors
/// `ODataError::InvalidCursor` if the token fails to serialize (not expected
/// in practice -- `CursorV1::encode`'s only failure mode is a `serde_json`
/// error over plain strings/enums).
pub fn encode(
    created_at: OffsetDateTime,
    id: Uuid,
    id_field: &'static str,
    binding: Option<String>,
) -> Result<String, ODataError> {
    let created_at_s = encode_cursor_value(
        &sea_orm::Value::TimeDateTimeWithTimeZone(Some(created_at)),
        FieldKind::DateTimeUtc,
    )
    .map_err(|_| ODataError::InvalidCursor)?;
    CursorV1 {
        k: vec![created_at_s, id.to_string()],
        o: SortDir::Desc,
        s: canonical_order(id_field).to_signed_tokens(),
        f: binding,
        d: FORWARD.to_owned(),
    }
    .encode()
    .map_err(|_| ODataError::InvalidCursor)
}

#[cfg(test)]
#[path = "pagination_tests.rs"]
mod pagination_tests;
