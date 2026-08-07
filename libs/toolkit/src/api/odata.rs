use axum::extract::{FromRequestParts, Query};
use axum::http::request::Parts;
use serde::Deserialize;
use toolkit_canonical_errors::CanonicalError;
use toolkit_odata::errors::OdataError;
use toolkit_odata::{CursorV1, Error as ODataError, ODataOrderBy, OrderKey, SortDir};

// Re-export types from toolkit-odata for convenience and better DX
pub use toolkit_odata::ODataQuery;
// CursorV1 is available through the private import above for internal use

/// Wire binding for the `OData` query-parameter family.
///
/// This struct is the single place where `OData` query parameters are bound
/// off the URL: `toolkit-odata` operates on already-parsed values and does no
/// HTTP query parsing of its own.
#[derive(Deserialize, Default)]
pub struct ODataParams {
    #[serde(rename = "$filter")]
    pub filter: Option<String>,
    #[serde(rename = "$orderby")]
    pub orderby: Option<String>,
    #[serde(rename = "$select")]
    pub select: Option<String>,
    /// Page size. Accepted as `limit` or as the canonical `OData` spelling
    /// `$top` (OASIS `OData` 4.01 Part 2: URL Conventions, §5.1.6 "System
    /// Query Options $top and $skip") — both spellings fold onto this one
    /// slot, so a gear needs no per-endpoint handling to honor either.
    ///
    /// Sending both in one request is ambiguous and is rejected with
    /// `400 InvalidArgument` (serde reports the second spelling as a
    /// duplicate field).
    #[serde(alias = "$top")]
    pub limit: Option<u64>,
    pub cursor: Option<String>,
}

pub const MAX_FILTER_LEN: usize = 8 * 1024;
pub const MAX_NODES: usize = 2000;
pub const MAX_ORDERBY_LEN: usize = 1024;
pub const MAX_ORDER_FIELDS: usize = 10;
pub const MAX_SELECT_LEN: usize = 2048;
pub const MAX_SELECT_FIELDS: usize = 100;

/// Build a canonical `InvalidArgument` keyed by the `$select` field.
fn select_invalid_arg(detail: impl Into<String>, reason: &'static str) -> CanonicalError {
    OdataError::invalid_argument()
        .with_field_violation("$select", detail, reason)
        .create()
}

/// Parse $select string into a list of field names.
/// Format: "field1, field2, field3, ..."
/// Field names are case-insensitive and whitespace is trimmed.
///
/// # Errors
/// Returns a `CanonicalError` if the select string is invalid. Axum renders it
/// via `IntoResponse for CanonicalError`; the `canonical_error_middleware`
/// fills `instance` / `trace_id` on the way out.
#[allow(clippy::result_large_err)]
pub fn parse_select(raw: &str) -> Result<Vec<String>, CanonicalError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(select_invalid_arg(
            "$select cannot be empty",
            "INVALID_SELECT",
        ));
    }

    if raw.len() > MAX_SELECT_LEN {
        return Err(select_invalid_arg("$select too long", "INVALID_SELECT"));
    }

    let fields: Vec<String> = raw
        .split(',')
        .map(|f| f.trim().to_lowercase())
        .filter(|f| !f.is_empty())
        .collect();

    if fields.is_empty() {
        return Err(select_invalid_arg(
            "$select must contain at least one field",
            "INVALID_SELECT",
        ));
    }

    if fields.len() > MAX_SELECT_FIELDS {
        return Err(select_invalid_arg(
            "$select contains too many fields",
            "INVALID_SELECT",
        ));
    }

    // Check for duplicate fields
    let mut seen = std::collections::HashSet::new();
    for field in &fields {
        if !seen.insert(field.clone()) {
            return Err(select_invalid_arg(
                format!("duplicate field in $select: {field}"),
                "INVALID_SELECT",
            ));
        }
    }

    Ok(fields)
}

/// Parse $orderby string into `ODataOrderBy`.
/// Format: "field1 [asc|desc], field2 [asc|desc], ..."
/// Default direction is asc if not specified.
///
/// # Errors
/// Returns `toolkit_odata::Error::InvalidOrderByField` if the orderby string is invalid.
pub fn parse_orderby(raw: &str) -> Result<ODataOrderBy, toolkit_odata::Error> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(ODataOrderBy::empty());
    }

    if raw.len() > MAX_ORDERBY_LEN {
        return Err(toolkit_odata::Error::InvalidOrderByField(
            "orderby too long".into(),
        ));
    }

    let mut keys = Vec::new();

    for part in raw.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }

        let tokens: Vec<&str> = part.split_whitespace().collect();
        let (field, dir) = match tokens.as_slice() {
            [field] | [field, "asc"] => (*field, SortDir::Asc),
            [field, "desc"] => (*field, SortDir::Desc),
            _ => {
                return Err(toolkit_odata::Error::InvalidOrderByField(format!(
                    "invalid orderby clause: {part}"
                )));
            }
        };

        if field.is_empty() {
            return Err(toolkit_odata::Error::InvalidOrderByField(
                "empty field name in orderby".into(),
            ));
        }

        keys.push(OrderKey {
            field: field.to_owned(),
            dir,
        });
    }

    if keys.len() > MAX_ORDER_FIELDS {
        return Err(toolkit_odata::Error::InvalidOrderByField(
            "too many order fields".into(),
        ));
    }

    Ok(ODataOrderBy(keys))
}

/// Build a canonical `InvalidArgument` for the `$filter` field.
fn filter_invalid_arg(detail: impl Into<String>, reason: &'static str) -> CanonicalError {
    OdataError::invalid_argument()
        .with_field_violation("$filter", detail, reason)
        .create()
}

/// Build a canonical `InvalidArgument` for an unspecified query parameter
/// (used for axum-level deserialization failures).
fn query_params_invalid_arg(detail: impl Into<String>) -> CanonicalError {
    OdataError::invalid_argument()
        .with_field_violation("query", detail, "INVALID_QUERY_PARAMS")
        .create()
}

/// Extract and validate full `OData` query from request parts.
/// - Parses $filter, $orderby, limit, cursor
/// - Enforces budgets and validates formats
/// - Returns unified `ODataQuery`
///
/// # Errors
/// Returns a `CanonicalError` if any `OData` parameter is invalid. Axum
/// renders it as `application/problem+json` via `IntoResponse for
/// CanonicalError`; `canonical_error_middleware` fills `instance` /
/// `trace_id` on the way out.
pub async fn extract_odata_query<S>(
    parts: &mut Parts,
    state: &S,
) -> Result<ODataQuery, CanonicalError>
where
    S: Send + Sync,
{
    let Query(params) = Query::<ODataParams>::from_request_parts(parts, state)
        .await
        .map_err(|e| query_params_invalid_arg(format!("Invalid query parameters: {e}")))?;

    let mut query = ODataQuery::new();

    // Parse filter
    if let Some(raw_filter) = params.filter.as_ref() {
        let raw = raw_filter.trim();
        if !raw.is_empty() {
            if raw.len() > MAX_FILTER_LEN {
                return Err(filter_invalid_arg("Filter too long", "FILTER_TOO_LONG"));
            }

            // Parse filter string using toolkit-odata
            let parsed = toolkit_odata::parse_filter_string(raw).map_err(|e| {
                // Length-only debug log; the canonical's `diagnostic()` carries
                // the actual parser cause for `canonical_error_middleware`.
                tracing::debug!(error = %e, filter_len = raw.len(), "OData filter parsing failed");
                CanonicalError::from(e)
            })?;

            if parsed.node_count() > MAX_NODES {
                tracing::debug!(
                    node_count = parsed.node_count(),
                    max_nodes = MAX_NODES,
                    "Filter complexity budget exceeded"
                );
                return Err(filter_invalid_arg(
                    "Filter too complex",
                    "FILTER_TOO_COMPLEX",
                ));
            }

            // Generate filter hash for cursor consistency (use non-consuming accessor)
            let filter_hash = toolkit_odata::pagination::short_filter_hash(Some(parsed.as_expr()));

            // Extract expression for query
            let core_expr = parsed.into_expr();

            query = query.with_filter(core_expr);
            if let Some(hash) = filter_hash {
                query = query.with_filter_hash(hash);
            }
        }
    }

    // Check for cursor+orderby conflict before parsing either
    if params.cursor.is_some() && params.orderby.is_some() {
        return Err(ODataError::OrderWithCursor.into());
    }

    // Parse cursor first (if present, skip orderby)
    if let Some(cursor_str) = params.cursor.as_ref() {
        let cursor = CursorV1::decode(cursor_str).map_err(|_| ODataError::InvalidCursor)?;
        query = query.with_cursor(cursor);
        // When cursor is present, order is empty (derived from cursor.s later)
        query = query.with_order(ODataOrderBy::empty());
    } else if let Some(raw_orderby) = params.orderby.as_ref() {
        // Parse orderby only when cursor is absent
        let order = parse_orderby(raw_orderby).map_err(CanonicalError::from)?;
        query = query.with_order(order);
    }

    // Parse limit
    if let Some(limit) = params.limit {
        if limit == 0 {
            return Err(ODataError::InvalidLimit.into());
        }
        query = query.with_limit(limit);
    }

    // Parse select
    if let Some(raw_select) = params.select.as_ref() {
        let fields = parse_select(raw_select)?;
        query = query.with_select(fields);
    }

    Ok(query)
}

use std::ops::Deref;

/// Simple Axum extractor for full `OData` query parameters.
/// Parses $filter, $orderby, limit, and cursor parameters.
/// Usage in handlers:
///   async fn `list_users(OData(query)`: `OData`, /* ... */) { /* use `query` */ }
#[derive(Debug, Clone)]
pub struct OData(pub ODataQuery);

impl OData {
    #[inline]
    pub fn into_inner(self) -> ODataQuery {
        self.0
    }
}

impl Deref for OData {
    type Target = ODataQuery;
    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<ODataQuery> for OData {
    #[inline]
    fn as_ref(&self) -> &ODataQuery {
        &self.0
    }
}

impl From<OData> for ODataQuery {
    #[inline]
    fn from(x: OData) -> Self {
        x.0
    }
}

impl<S> FromRequestParts<S> for OData
where
    S: Send + Sync,
{
    type Rejection = CanonicalError;

    #[allow(clippy::manual_async_fn)]
    fn from_request_parts(
        parts: &mut Parts,
        state: &S,
    ) -> impl core::future::Future<Output = Result<Self, Self::Rejection>> + Send {
        async move {
            let query = extract_odata_query(parts, state).await?;
            Ok(OData(query))
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "odata_tests.rs"]
mod odata_tests;
