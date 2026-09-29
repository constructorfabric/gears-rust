//! Cursor codec shared by the gear's three SQL-backed keyset listings
//! (`GET /files`, `GET /files/{id}/versions`, `GET /retention-rules`, per
//! `guidelines/DNA/REST/QUERYING.md`).
//!
//! All three share one page shape: `created_at DESC` then an id-column
//! tie-breaker, also `DESC` (newest first -- the same order the previous
//! offset pagination already used). `items` are always returned in this
//! same canonical order regardless of navigation direction
//! (`guidelines/DNA/REST/QUERYING.md`'s "never reverse for backward
//! navigation"). Building/parsing the opaque token itself lives here so the
//! REST handlers, the domain services, and the SDK's in-process local
//! client all share one codec instead of each re-deriving the `CursorV1`
//! shape; the repo layer only ever sees a decoded [`Seek`] (typed
//! `created_at`/id values plus a [`Direction`]), never the wire token.
//!
//! Both directions are supported: a `next_cursor` seeks *forward* (older
//! rows, in canonical order) and a `prev_cursor` seeks *backward* (newer
//! rows) from the position it encodes. The direction travels inside the
//! cursor itself (`CursorV1::d`, `"fwd"`/`"bwd"`) -- a client never picks it
//! directly, only replays whichever of `next_cursor`/`prev_cursor` a
//! previous response gave it (see [`finish_page`] for how both are built).
//! Same as a forward cursor, a `"bwd"` token carries no extra
//! confidentiality/integrity guarantee beyond the binding check `decode`
//! already performs for every direction -- `CursorV1` was never
//! cryptographically signed, so this changes no security property.

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
/// Every cursor this module builds carries its own direction (`d` on the
/// wire): a `next_cursor` is always [`Direction::Forward`], a `prev_cursor`
/// always [`Direction::Backward`] -- see [`finish_page`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Resume strictly after this position, in canonical order (i.e. seek
    /// toward older rows under this module's `created_at DESC, id DESC`
    /// canonical order) -- what `next_cursor` seeks from.
    Forward,
    /// Resume strictly before this position, in canonical order (seek
    /// toward newer rows) -- what `prev_cursor` seeks from. The repo layer
    /// runs this as a keyset query in the *opposite* order (ascending,
    /// closest-to-the-cursor-first) and reverses the result back to
    /// canonical order before it ever reaches a caller -- see
    /// [`finish_page`]'s doc comment.
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

/// The decoded, already-validated position of the last (or, for a backward
/// cursor, first) row a previous page returned -- everything a repo's
/// keyset predicate needs to seek past it, plus which way to seek.
#[derive(Debug, Clone, Copy)]
pub struct Seek {
    pub created_at: OffsetDateTime,
    pub id: Uuid,
    pub direction: Direction,
}

/// Decode and validate a cursor token against one listing's canonical order
/// and binding.
///
/// # Errors
/// `ODataError::InvalidCursor`/`CursorInvalid*` for an unreadable token
/// (`CursorInvalidDirection` specifically for a `d` that is neither `"fwd"`
/// nor `"bwd"` -- rejected by `CursorV1::decode` itself, before this
/// function ever sees it); `OrderMismatch` for a token whose `s` doesn't
/// match `id_field`'s canonical order; `FilterMismatch` for a token bound to
/// a different owner/file (or a bound token replayed against an unbound
/// listing, or vice versa).
pub fn decode(
    token: &str,
    id_field: &'static str,
    binding: Option<&str>,
) -> Result<Seek, ODataError> {
    let cursor = CursorV1::decode(token)?;
    // `CursorV1::decode` already rejects any `d` other than `"fwd"`/`"bwd"`
    // (`ODataError::CursorInvalidDirection`), so `cursor.d` is always
    // exactly one of the two by this point.
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

/// Encode a cursor seeking from `(created_at, id)` in `direction` -- pass
/// [`Direction::Forward`] to build a `next_cursor`, [`Direction::Backward`]
/// for a `prev_cursor`. [`finish_page`] is the one place that should
/// ordinarily need to call this directly; a handful of call sites that
/// rebuild a `next_cursor` after further, cursor-oblivious truncation (e.g.
/// `read_ops::list_versions_with_manifests`'s manifest-byte budget) call it
/// directly too, always with `Direction::Forward` for that same reason.
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

/// Trim an over-fetched page (`limit + 1` rows, as the repo query returned
/// them) down to `limit`, restore canonical order when the query ran
/// backward, and build both `next_cursor`/`prev_cursor` -- shared by all
/// three keyset listings so each `Store` method differs only in *which*
/// repo query it calls, never in how the raw rows become a `Page`.
///
/// `rows` must be exactly what the repo query returned, in whichever order
/// that query actually ran: canonical (`created_at DESC, id DESC`) for a
/// forward query (`after` is `None` or `Some(Seek { direction: Forward,
/// .. })`), or the *reverse* of canonical (ascending) for a backward one
/// (`after` is `Some(Seek { direction: Backward, .. })`) -- see
/// [`Direction::Backward`]'s own doc comment for why the repo layer runs a
/// backward page as an ascending query. This function reverses a backward
/// result back to canonical order itself, so every caller downstream of it
/// (including this gear's REST handlers and the SDK) only ever sees
/// canonical-order `items`, matching `guidelines/DNA/REST/QUERYING.md`'s
/// "never reverse for backward navigation".
///
/// Both cursors follow the same rule regardless of direction: `next_cursor`
/// is built from `rows`' last item (after restoring canonical order) when
/// there is a further forward page, `prev_cursor` from its first item when
/// there is a further backward page. Which one of those is "further" vs.
/// "where this page's own cursor came from" depends on `after`'s direction:
/// - Forward query: over-fetching past `limit` rows (`rows.len() >
///   limit` before trimming) directly answers "is there a further forward
///   page" (`next_cursor`); `after.is_some()` alone answers "is there a
///   page before this one" (`prev_cursor`) -- the seek position `after`
///   decoded from is itself proof that at least one row precedes this page.
/// - Backward query: symmetric. Over-fetching answers `prev_cursor`;
///   `after.is_some()` answers `next_cursor`.
///
/// An empty result page builds neither cursor -- there is no row to encode
/// either one from, and `guidelines/DNA/REST/QUERYING.md` allows omitting
/// both.
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
        // The repo query ran ascending (closest-to-cursor first) to make
        // the keyset predicate sargable; restore the canonical descending
        // order every caller downstream of this function expects.
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
