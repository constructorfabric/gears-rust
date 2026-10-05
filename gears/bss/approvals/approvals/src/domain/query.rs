//! List and counts parameters. `$orderby` beside a cursor is refused before the cursor is read.

use std::collections::BTreeMap;

use bss_approvals_sdk::{Order, SortKey, SourceNarrowing};
use toolkit_canonical_errors::CanonicalError;
use toolkit_odata::Error as ODataError;
use toolkit_odata::SortDir;
use uuid::Uuid;

use super::cursor;

/// Default page size.
pub const DEFAULT_LIMIT: u32 = 50;
/// The largest page the inbox asks for.
pub const MAX_LIMIT: u32 = 200;

/// The list query, after the HTTP layer has parsed it.
#[derive(Debug, Clone, Default)]
pub struct ListParams {
    /// Unit state.
    pub state: Option<String>,
    /// Unit kind.
    pub kind: Option<String>,
    /// Referenced aggregate.
    pub ref_id: Option<Uuid>,
    /// Book alias. The source decides what it keeps.
    pub book_id: Option<Uuid>,
    /// Page size. Absent means [`DEFAULT_LIMIT`].
    pub limit: Option<u64>,
    /// Continuation token.
    pub cursor: Option<String>,
    /// `submitted_at` direction. Absent means newest first.
    pub orderby: Option<String>,
    /// Absent means the live impact is read.
    pub impact: Option<bool>,
}

/// A list call the sources can answer.
#[derive(Debug, Clone)]
pub struct PreparedList {
    /// The narrowing sent to every source.
    pub narrowing: SourceNarrowing,
    /// The order of this page. A cursor carries its own.
    pub order: Order,
    /// Page size, clamped at [`MAX_LIMIT`].
    pub limit: u32,
    /// Whether sources fill `impact`.
    pub impact: bool,
    /// Identity of `narrowing`.
    pub hash: String,
    /// Per-source keys from the cursor. An absent source starts at `None`.
    pub keys: BTreeMap<String, Option<SortKey>>,
    /// Sources the cursor recorded as down. A continuation does not ask them.
    pub unavailable: Vec<String>,
}

/// Checks the list query and builds the call.
///
/// # Errors
/// 400 `ORDER_WITH_CURSOR`, `INVALID_ORDERBY_FIELD`, `INVALID_LIMIT`, `INVALID_CURSOR`, or
/// `FILTER_MISMATCH`.
pub fn prepare_list(params: &ListParams) -> Result<PreparedList, CanonicalError> {
    if params.cursor.is_some() && params.orderby.is_some() {
        return Err(ODataError::OrderWithCursor.into());
    }
    let narrowing = narrowing_of(params);
    let hash = cursor::narrowing_hash(&narrowing);
    let limit = page_limit(params.limit)?;
    let (order, keys, unavailable) = match params.cursor.as_deref() {
        Some(token) => {
            let decoded = cursor::decode(token)?;
            if decoded.narrowing_hash != hash {
                return Err(ODataError::FilterMismatch.into());
            }
            (decoded.order, decoded.keys, decoded.unavailable)
        }
        None => (
            parse_order(params.orderby.as_deref())?,
            BTreeMap::new(),
            Vec::new(),
        ),
    };
    Ok(PreparedList {
        narrowing,
        order,
        limit,
        impact: params.impact.unwrap_or(true),
        hash,
        keys,
        unavailable,
    })
}

/// The counts query's narrowing. It takes nothing else.
#[must_use]
pub fn narrowing_of(params: &ListParams) -> SourceNarrowing {
    SourceNarrowing {
        state: params.state.clone(),
        kind: params.kind.clone(),
        ref_id: params.ref_id,
        book_id: params.book_id,
    }
}

fn page_limit(raw: Option<u64>) -> Result<u32, CanonicalError> {
    let Some(raw) = raw else {
        return Ok(DEFAULT_LIMIT);
    };
    if raw == 0 {
        return Err(ODataError::InvalidLimit.into());
    }
    let value = u32::try_from(raw).unwrap_or(MAX_LIMIT);
    Ok(value.min(MAX_LIMIT))
}

fn parse_order(raw: Option<&str>) -> Result<Order, CanonicalError> {
    let Some(raw) = raw else {
        return Ok(Order::Desc);
    };
    let parsed = toolkit::api::odata::parse_orderby(raw)?;
    match parsed.0.as_slice() {
        [] => Ok(Order::Desc),
        [key] if key.field == "submitted_at" => Ok(match key.dir {
            SortDir::Asc => Order::Asc,
            SortDir::Desc => Order::Desc,
        }),
        _ => Err(ODataError::InvalidOrderByField(raw.to_owned()).into()),
    }
}
