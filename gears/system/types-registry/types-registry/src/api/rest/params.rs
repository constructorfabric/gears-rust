//! Query-string guards for the v2 read routes (SPEC §10.2).
//!
//! axum drops unclaimed keys and `ToolKit` refuses only unknown `$` options, so an
//! undeclared parameter would otherwise be answered as if it had not been sent.

use axum::extract::{FromRequestParts, Query};
use axum::http::request::Parts;
use toolkit::api::odata::{ODataQuery, extract_odata_query};
use toolkit_canonical_errors::CanonicalError;

use super::error::{
    depth_not_recognized, duplicate_query_param, kind_not_recognized,
    lifecycle_status_not_recognized, page_size_zero, pattern_too_long, query_params_unreadable,
    too_many_query_params, unsupported_query_params,
};
use super::select;
use std::num::NonZeroU8;

use toolkit_odata::CursorV1;

use crate::domain::enums::{EntityKind, LifecycleFilter};
use crate::domain::selection::FieldSelection;

/// Parameters `GET /entities/{entity_key}` accepts.
pub const EXACT_READ: &[&str] = &["$select"];

/// `POST /entities:batchGet` carries everything in its body, `$select` included.
pub const BATCH_READ: &[&str] = &[];

/// A refusal names at most this many unknown keys, which also bounds the dedup.
const MAX_REPORTED_KEYS: usize = 16;

fn guard(pairs: &[(String, String)], allowed: &[&str]) -> Result<(), CanonicalError> {
    let mut unsupported: Vec<&str> = Vec::new();
    for (key, _) in pairs {
        if !allowed.contains(&key.as_str()) && !unsupported.contains(&key.as_str()) {
            unsupported.push(key);
            if unsupported.len() == MAX_REPORTED_KEYS {
                break;
            }
        }
    }
    if !unsupported.is_empty() {
        return Err(unsupported_query_params(&unsupported, allowed));
    }
    for (i, (key, _)) in pairs.iter().enumerate() {
        if pairs[..i].iter().any(|(earlier, _)| earlier == key) {
            return Err(duplicate_query_param(key));
        }
    }
    Ok(())
}

/// Above any valid request: each route's vocabulary is smaller and repeats are refused.
const MAX_QUERY_PAIRS: usize = 32;

async fn raw_pairs<S: Send + Sync>(
    parts: &mut Parts,
    state: &S,
) -> Result<Vec<(String, String)>, CanonicalError> {
    // Counted on the raw string, before any pair is decoded or allocated.
    let count = parts.uri.query().map_or(0, |q| q.split('&').count());
    if count > MAX_QUERY_PAIRS {
        return Err(too_many_query_params(count, MAX_QUERY_PAIRS));
    }
    let Query(pairs) = Query::<Vec<(String, String)>>::from_request_parts(parts, state)
        .await
        .map_err(|e| query_params_unreadable(&e.to_string()))?;
    Ok(pairs)
}

/// # Errors
/// A `400` for an unsupported, repeated or malformed parameter.
pub async fn extract<S: Send + Sync>(
    parts: &mut Parts,
    state: &S,
    allowed: &[&str],
) -> Result<(ODataQuery, FieldSelection), CanonicalError> {
    let pairs = raw_pairs(parts, state).await?;
    guard(&pairs, allowed)?;
    odata(parts, state, &pairs).await
}

/// `ToolKit`'s `OData` extraction, over `pairs` the caller has already guarded.
async fn odata<S: Send + Sync>(
    parts: &mut Parts,
    state: &S,
    pairs: &[(String, String)],
) -> Result<(ODataQuery, FieldSelection), CanonicalError> {
    if let Some((_, raw)) = pairs.iter().find(|(key, _)| key == "$select") {
        select::check_raw(raw)?;
    }
    let query = extract_odata_query(parts, state).await?;
    let selection = select::from_names(query.selected_fields())?;
    Ok((query, selection))
}

/// The exact read's one query parameter, `$select`.
pub struct ExactReadSelection(pub FieldSelection);

impl<S: Send + Sync> FromRequestParts<S> for ExactReadSelection {
    type Rejection = CanonicalError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let (_, selection) = extract(parts, state, EXACT_READ).await?;
        Ok(Self(selection))
    }
}

/// `:batchGet` takes no query parameters; its `$select` is a body field.
pub struct NoQuery;

impl<S: Send + Sync> FromRequestParts<S> for NoQuery {
    type Rejection = CanonicalError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        guard(&raw_pairs(parts, state).await?, BATCH_READ)?;
        Ok(Self)
    }
}

/// Parameters `GET /entities` accepts; `limit`/`$top` and `cursor`/`$skiptoken`
/// are `ToolKit`'s two spellings of one slot each.
pub const DISCOVERY: &[&str] = &[
    "pattern",
    "depth",
    "kind",
    "lifecycle_status",
    "limit",
    "$top",
    "cursor",
    "$skiptoken",
    "$select",
];

/// The discovery query, validated but with its cursor still unbound: the binding
/// needs the normalized selection, which only exists after extraction.
pub struct DiscoveryParams {
    pub pattern: Option<String>,
    pub kind: Option<EntityKind>,
    pub lifecycle: LifecycleFilter,
    pub max_chain_depth: Option<NonZeroU8>,
    pub limit: Option<u64>,
    pub cursor: Option<CursorV1>,
    pub selection: FieldSelection,
}

/// Plain decimal digits only: `NonZeroU8::from_str` would also take `+5`.
fn parse_depth(raw: &str) -> Result<NonZeroU8, CanonicalError> {
    if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return Err(depth_not_recognized(raw));
    }
    raw.parse().map_err(|_| depth_not_recognized(raw))
}

/// The wire spellings of [`EntityKind`]; a test pins them to `EntityKindDto`.
fn parse_kind(raw: &str) -> Result<EntityKind, CanonicalError> {
    match raw {
        "type_schema" => Ok(EntityKind::TypeSchema),
        "instance" => Ok(EntityKind::Instance),
        _ => Err(kind_not_recognized(raw)),
    }
}

fn parse_lifecycle(raw: &str) -> Result<LifecycleFilter, CanonicalError> {
    match raw {
        "active" => Ok(LifecycleFilter::Active),
        "deleted" => Ok(LifecycleFilter::Deleted),
        "all" => Ok(LifecycleFilter::All),
        _ => Err(lifecycle_status_not_recognized(raw)),
    }
}

fn value<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

/// One slot under either spelling; both at once is ambiguous.
fn slot<'a>(
    pairs: &'a [(String, String)],
    names: [&'static str; 2],
) -> Result<Option<(&'static str, &'a str)>, CanonicalError> {
    match names.map(|name| value(pairs, name).map(|v| (name, v))) {
        [Some(_), Some(_)] => Err(duplicate_query_param(names[1])),
        [first, second] => Ok(first.or(second)),
    }
}

impl<S: Send + Sync> FromRequestParts<S> for DiscoveryParams {
    type Rejection = CanonicalError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let pairs = raw_pairs(parts, state).await?;
        guard(&pairs, DISCOVERY)?;
        let limit = slot(&pairs, ["limit", "$top"])?;
        let cursor = slot(&pairs, ["cursor", "$skiptoken"])?;
        // Checked before ToolKit's extraction so the refusal names the spelling
        // the caller used and carries the gear's cursor diagnostics.
        if let Some((name, "0")) = limit {
            return Err(page_size_zero(name));
        }
        let cursor = cursor
            .map(|(_, token)| super::cursor::read(token))
            .transpose()?;

        let (query, selection) = odata(parts, state, &pairs).await?;
        let pattern = value(&pairs, "pattern").map(str::to_owned);
        if let Some(p) = &pattern
            && p.len() > 1024
        {
            return Err(pattern_too_long(p.len()));
        }
        Ok(Self {
            pattern,
            kind: value(&pairs, "kind").map(parse_kind).transpose()?,
            lifecycle: value(&pairs, "lifecycle_status")
                .map(parse_lifecycle)
                .transpose()?
                .unwrap_or_default(),
            max_chain_depth: value(&pairs, "depth").map(parse_depth).transpose()?,
            limit: query.limit,
            cursor,
            selection,
        })
    }
}

#[cfg(test)]
#[path = "params_tests.rs"]
mod tests;
