//! Cursor codec shared by the three keyset listings (`GET /files`,
//! `GET /files/{id}/versions`, `GET /retention-rules`).
//!
//! Order is `created_at DESC` then an id tie-breaker, also `DESC`; `items` always come back in
//! this canonical order, even for backward navigation. The repo layer only sees a decoded
//! `Seek`, never the wire token.
//!
//! The direction (`"fwd"`/`"bwd"`) travels inside the cursor (`CursorV1::d`): `next_cursor`
//! seeks older rows, `prev_cursor` newer ones. Cursors are not signed; `decode` only checks
//! the order and binding.

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

/// Wire value for [`Direction::Forward`].
const FORWARD: &str = "fwd";
/// Wire value for [`Direction::Backward`].
const BACKWARD: &str = "bwd";

/// Which way a keyset query seeks relative to a decoded cursor's position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Resume strictly after this position in canonical order (older rows); used by `next_cursor`.
    Forward,
    /// Resume strictly before this position (newer rows); used by `prev_cursor`. The repo runs
    /// it as an ascending query and `finish_page` reverses the rows back to canonical order.
    Backward,
}

impl Direction {
    fn as_wire(self) -> &'static str {
        match self {
            Self::Forward => FORWARD,
            Self::Backward => BACKWARD,
        }
    }
}

/// The keyset shape of every listing: `created_at` then the listing's id column, both descending.
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

/// Stable fingerprint of what a cursor is bound to (owner pair for `/files`, `file_id` for
/// versions; none for retention rules). Uses `toolkit_odata`'s FNV hash because
/// `std::hash::DefaultHasher` is not stable across process runs.
fn binding_hash(raw: &str) -> Option<String> {
    let expr = toolkit_odata::ast::Expr::Value(toolkit_odata::ast::Value::String(raw.to_owned()));
    toolkit_odata::pagination::short_filter_hash(Some(&expr))
}

/// `/files` binding: a cursor cannot resume a listing of a different owner pair.
#[must_use]
pub fn files_binding(owner: &OwnerFilter) -> Option<String> {
    binding_hash(&format!("{}:{}", owner.owner_kind.as_str(), owner.owner_id))
}

/// `/files/{id}/versions` binding: a cursor cannot resume a listing of another file.
#[must_use]
pub fn versions_binding(file_id: Uuid) -> Option<String> {
    binding_hash(&file_id.to_string())
}

/// Decoded position of the last (backward: first) row of the previous page, plus seek direction.
#[derive(Debug, Clone, Copy)]
pub struct Seek {
    pub created_at: OffsetDateTime,
    pub id: Uuid,
    pub direction: Direction,
}

/// Decode and validate a cursor token against one listing's canonical order and binding.
///
/// # Errors
/// `ODataError::InvalidCursor`/`CursorInvalid*` for an unreadable token or bad direction;
/// `OrderMismatch` for a different canonical order; `FilterMismatch` for a different binding.
pub fn decode(
    token: &str,
    id_field: &'static str,
    binding: Option<&str>,
) -> Result<Seek, ODataError> {
    let cursor = CursorV1::decode(token)?;
    // `CursorV1::decode` already rejects any `d` other than `"fwd"`/`"bwd"`.
    let direction = if cursor.d == BACKWARD {
        Direction::Backward
    } else {
        Direction::Forward
    };
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
    Ok(Seek {
        created_at,
        id,
        direction,
    })
}

/// Resolve `?limit`: `None` gives `default`, values above `max` are clamped to `max`, and
/// `Some(0)` is refused.
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

/// Encode a cursor seeking from `(created_at, id)` in `direction`.
///
/// # Errors
/// `ODataError::InvalidCursor` if the token fails to serialize.
pub fn encode(
    created_at: OffsetDateTime,
    id: Uuid,
    id_field: &'static str,
    binding: Option<String>,
    direction: Direction,
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
        d: direction.as_wire().to_owned(),
    }
    .encode()
    .map_err(|_| ODataError::InvalidCursor)
}

/// Trim an over-fetched page (`limit + 1` rows) to `limit`, restore canonical order for a
/// backward query, and build `next_cursor`/`prev_cursor`.
///
/// `rows` are in the order the repo query ran: canonical for a forward query, ascending for a
/// backward one (`after` carries `Direction::Backward`).
///
/// For a forward query, over-fetching means a further forward page exists (`next_cursor`), and
/// `after.is_some()` proves an earlier page exists (`prev_cursor`). A backward query is
/// symmetric. An empty page builds neither cursor.
///
/// # Errors
/// Whatever [`encode`] returns for a token that fails to serialize.
pub fn finish_page<T>(
    mut rows: Vec<T>,
    limit: u64,
    after: Option<Seek>,
    id_field: &'static str,
    binding: Option<&str>,
    key_of: impl Fn(&T) -> (OffsetDateTime, Uuid),
) -> Result<toolkit_odata::Page<T>, ODataError> {
    let direction = after.map_or(Direction::Forward, |s| s.direction);
    let overfetched = rows.len() as u64 > limit;
    if overfetched {
        rows.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    }
    if direction == Direction::Backward {
        // The repo ran ascending to keep the keyset predicate sargable; restore canonical order.
        rows.reverse();
    }

    let (next_cursor, prev_cursor) = match (rows.first(), rows.last()) {
        (Some(first), Some(last)) => {
            let (further_forward, further_backward) = match direction {
                Direction::Forward => (overfetched, after.is_some()),
                Direction::Backward => (after.is_some(), overfetched),
            };
            let next = further_forward
                .then(|| {
                    let (created_at, id) = key_of(last);
                    encode(
                        created_at,
                        id,
                        id_field,
                        binding.map(str::to_owned),
                        Direction::Forward,
                    )
                })
                .transpose()?;
            let prev = further_backward
                .then(|| {
                    let (created_at, id) = key_of(first);
                    encode(
                        created_at,
                        id,
                        id_field,
                        binding.map(str::to_owned),
                        Direction::Backward,
                    )
                })
                .transpose()?;
            (next, prev)
        }
        _ => (None, None),
    };

    Ok(toolkit_odata::Page::new(
        rows,
        toolkit_odata::PageInfo {
            next_cursor,
            prev_cursor,
            limit,
        },
    ))
}

#[cfg(test)]
#[path = "pagination_tests.rs"]
mod pagination_tests;
