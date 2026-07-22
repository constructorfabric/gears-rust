//! Order-by rendering, keyset (tuple-comparison) predicates, and cursor
//! encode/decode for keyset pagination.
//!
//! All column identifiers come from a caller-supplied allowlist closure
//! (`record_column` / `usage_type_column` from [`super::translate`]); cursor
//! key values are always bound. The v1 gateway default order is the
//! all-ascending `(created_at, id)` tuple, so [`keyset_predicate`] emits the
//! row-value tuple form for uniform-direction orders and rejects mixed
//! directions (documented limitation — see that fn).
//!
//! # Verified `toolkit-odata` cursor / order API (Task E1)
//!
//! - `ODataOrderBy(pub Vec<OrderKey>)`; `OrderKey { field: String, dir:
//!   SortDir }`; `SortDir::{Asc, Desc}` with `reverse()`. `ODataOrderBy` has
//!   `to_signed_tokens() -> String` (`"+created_at,+id"`) and
//!   `from_signed_tokens(&str) -> Result<Self, toolkit_odata::Error>`.
//! - `CursorV1 { k: Vec<String>, o: SortDir, s: String, f: Option<String>, d:
//!   String }`; `encode(&self) -> serde_json::Result<String>` (base64url);
//!   `decode(token: &str) -> Result<CursorV1, toolkit_odata::Error>`. `d` is
//!   `"fwd"` / `"bwd"`.
//! - `Page::new(items: Vec<T>, page_info: PageInfo)`; `PageInfo { next_cursor:
//!   Option<String>, prev_cursor: Option<String>, limit: u64 }`.

use std::str::FromStr;

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use toolkit_odata::filter::FieldKind;
use toolkit_odata::{CursorV1, ODataOrderBy, SortDir};

use super::bind::SqlBind;
use super::translate::SqlCtx;

/// Reject any cursor whose direction is not forward (`"fwd"`).
///
/// v1 mints and supports only forward cursors. [`keyset_predicate`] derives the
/// `>`/`<` comparison operator from the sort direction, **not** from the
/// cursor's `d` field, so a `"bwd"` cursor would be silently walked forward and
/// return the wrong page. Reject it fail-closed until backward paging is
/// actually implemented.
///
/// # Errors
///
/// Returns an error string when `cursor.d` is anything other than `"fwd"`.
pub fn ensure_forward_cursor(cursor: &CursorV1) -> Result<(), String> {
    if cursor.d == "fwd" {
        Ok(())
    } else {
        Err(format!(
            "unsupported cursor direction `{}`: only forward paging is supported",
            cursor.d
        ))
    }
}

/// Render an `ORDER BY` column list (`"created_at ASC, id ASC"`) from an
/// `ODataOrderBy`, resolving each field through `col`.
///
/// # Errors
///
/// Returns an error string when the order is empty, or when a field is not on
/// the allowlist (never interpolated).
pub fn render_order_by(
    order: &ODataOrderBy,
    col: impl Fn(&str) -> Option<&'static str>,
) -> Result<String, String> {
    if order.is_empty() {
        return Err("order must not be empty".to_owned());
    }
    let parts = order
        .0
        .iter()
        .map(|key| {
            let column = col(&key.field)
                .ok_or_else(|| format!("order field not allowlisted: {}", key.field))?;
            let dir = match key.dir {
                SortDir::Asc => "ASC",
                SortDir::Desc => "DESC",
            };
            Ok(format!("{column} {dir}"))
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(parts.join(", "))
}

/// Build a keyset predicate as a row-value tuple comparison for the supplied
/// `(field_name, is_ascending)` order pairs against `cursor_keys`.
///
/// For an all-ascending order this is `(c1, c2, …) > ($a, $b, …)`; for an
/// all-descending order, `(…) < (…)`. Each cursor key is parsed to a typed bind
/// via [`cursor_key_to_bind`] — keyed by the field's declared [`FieldKind`]
/// (resolved through `kind`), not its column name — and pushed onto `ctx`.
///
/// # v1 limitation
///
/// Only uniform-direction orders are supported. The v1 gateway always sorts
/// `(created_at, id)` ascending, so the tuple form covers the live path;
/// mixed-direction orders return an error (the lexicographic OR-form is not
/// emitted in v1).
///
/// # NULL safety
///
/// The row-value tuple comparison is only sound when every tuple column is
/// `NOT NULL`: in SQL three-valued logic a tuple whose column is NULL compares
/// as NULL, so a NULL-keyed row is silently dropped from the page (and a page
/// ending on such a row cannot encode a `next_cursor`). `keyset_safe` is the
/// caller's fail-closed predicate for "this field maps to a never-null column";
/// any field it rejects fails the whole predicate closed. The gateway already
/// rejects a caller `$orderby` on a nullable field with a `400`, so this is the
/// defence-in-depth backstop for a crafted cursor or a future in-process
/// caller.
///
/// # Errors
///
/// Returns an error string when `order_pairs` is empty, its length differs from
/// `cursor_keys`, a field is not keyset-safe (nullable), a field is not on the
/// allowlist, a field has no known kind, the directions are mixed, or a cursor
/// key cannot be parsed.
pub fn keyset_predicate(
    order_pairs: &[(&str, bool)],
    cursor_keys: &[String],
    col: impl Fn(&str) -> Option<&'static str>,
    kind: impl Fn(&str) -> Option<FieldKind>,
    keyset_safe: impl Fn(&str) -> bool,
    ctx: &mut SqlCtx,
) -> Result<String, String> {
    if order_pairs.is_empty() {
        return Err("keyset order must not be empty".to_owned());
    }
    if order_pairs.len() != cursor_keys.len() {
        return Err(format!(
            "cursor key count {} does not match order arity {}",
            cursor_keys.len(),
            order_pairs.len()
        ));
    }

    let all_asc = order_pairs.iter().all(|(_, asc)| *asc);
    let all_desc = order_pairs.iter().all(|(_, asc)| !*asc);
    let cmp = if all_asc {
        ">"
    } else if all_desc {
        "<"
    } else {
        return Err("mixed-direction keyset orders are unsupported in v1".to_owned());
    };

    let mut columns = Vec::with_capacity(order_pairs.len());
    let mut placeholders = Vec::with_capacity(order_pairs.len());
    for ((field, _), raw) in order_pairs.iter().zip(cursor_keys.iter()) {
        if !keyset_safe(field) {
            return Err(format!(
                "keyset field is nullable and cannot be a keyset ordering key: {field}"
            ));
        }
        let column = col(field).ok_or_else(|| format!("keyset field not allowlisted: {field}"))?;
        let field_kind =
            kind(field).ok_or_else(|| format!("keyset field has no known kind: {field}"))?;
        let bind = cursor_key_to_bind(field_kind, raw)?;
        let n = ctx.push(bind);
        columns.push(column);
        placeholders.push(format!("${n}"));
    }

    Ok(format!(
        "({}) {cmp} ({})",
        columns.join(", "),
        placeholders.join(", ")
    ))
}

/// Parse a raw cursor key string into a typed bind according to the keyset
/// field's declared [`FieldKind`] — not its column name, so a new keyset column
/// gets the correct bind type for free and an unsupported one fails loudly
/// rather than silently binding as text (a `column op text` runtime error).
///
/// Every keyset-eligible column today is `Uuid`, `DateTimeUtc`, or `String`;
/// any other kind returns an error (fail-closed) until keyset support for it is
/// deliberately added. (`SqlCtx::push` is private, so callers go through
/// [`keyset_predicate`]; this helper is exposed for testing and reuse.)
///
/// # Errors
///
/// Returns an error string when the value cannot be parsed for its kind, or when
/// the kind is not supported as a keyset column.
pub fn cursor_key_to_bind(kind: FieldKind, raw: &str) -> Result<SqlBind, String> {
    match kind {
        FieldKind::DateTimeUtc => OffsetDateTime::parse(raw, &Rfc3339)
            .map(SqlBind::DateTime)
            .map_err(|e| format!("invalid datetime cursor key `{raw}`: {e}")),
        FieldKind::Uuid => Uuid::from_str(raw)
            .map(SqlBind::Uuid)
            .map_err(|e| format!("invalid uuid cursor key `{raw}`: {e}")),
        FieldKind::String => Ok(SqlBind::Str(raw.to_owned())),
        other => Err(format!(
            "cursor key kind `{other}` is not supported as a keyset column"
        )),
    }
}

/// Build and encode the forward (`"fwd"`) cursor for the next page from the
/// last in-page row's key values, in `order` field order.
///
/// `s` carries the signed sort tokens (`"+created_at,+id"`); `o` is the
/// primary sort direction; `f` carries the optional filter hash for
/// consistency checks on decode.
///
/// # Errors
///
/// Returns an error string when the order is empty, its arity differs from
/// `last_row_keys`, or serialization fails.
pub fn encode_next_cursor(
    order: &ODataOrderBy,
    last_row_keys: &[String],
    filter_hash: Option<&str>,
) -> Result<String, String> {
    if order.is_empty() {
        return Err("cursor order must not be empty".to_owned());
    }
    if order.0.len() != last_row_keys.len() {
        return Err(format!(
            "row key count {} does not match order arity {}",
            last_row_keys.len(),
            order.0.len()
        ));
    }
    let primary_dir = order.0.first().map_or(SortDir::Asc, |k| k.dir);
    let cursor = CursorV1 {
        k: last_row_keys.to_vec(),
        o: primary_dir,
        s: order.to_signed_tokens(),
        f: filter_hash.map(str::to_owned),
        d: "fwd".to_owned(),
    };
    cursor
        .encode()
        .map_err(|e| format!("cursor encode failed: {e}"))
}

/// Decode a cursor token. Thin wrapper over [`CursorV1::decode`].
///
/// # Errors
///
/// Returns the `toolkit_odata::Error` surfaced by [`CursorV1::decode`]
/// (malformed base64 / JSON / version / direction / keys).
pub fn decode_cursor(token: &str) -> Result<CursorV1, toolkit_odata::Error> {
    CursorV1::decode(token)
}
