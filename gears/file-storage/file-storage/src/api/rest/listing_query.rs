//! Query-string extractor shared by the four cursor-paginated listings.
//!
//! It examines every query key before anything is parsed, so one response carries all the
//! violations of a request, and the response depends on which keys are present, never on their
//! values. Page parameters themselves are then parsed by the platform's
//! `toolkit::api::odata::extract_odata_query`.

use std::marker::PhantomData;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use toolkit::api::odata::{ACCEPTED_SYSTEM_QUERY_OPTIONS, extract_odata_query};
use toolkit::api::rest::extract::Query;
use toolkit_canonical_errors::CanonicalError;
use toolkit_odata::ODataQuery;
use toolkit_odata::errors::OdataError;

/// Reason code of every rejected listing query key.
const UNSUPPORTED_QUERY_PARAM: &str = "UNSUPPORTED_QUERY_PARAM";
/// Reason code of a parameter given more than once, repeated or under both spellings.
const INVALID_QUERY_PARAMS: &str = "INVALID_QUERY_PARAMS";

/// The plain (non-`$`) query keys a listing accepts, besides the `$` options of `OData`.
pub trait ListingKeys {
    const KEYS: &'static [&'static str];
}

/// Keys of `GET /files`.
#[derive(Debug)]
pub struct FilesKeys;

impl ListingKeys for FilesKeys {
    const KEYS: &'static [&'static str] = &["owner_kind", "owner_id", "limit", "cursor"];
}

/// Keys of the listings that take only page parameters.
#[derive(Debug)]
pub struct PageKeys;

impl ListingKeys for PageKeys {
    const KEYS: &'static [&'static str] = &["limit", "cursor"];
}

/// A listing's parsed page query plus its plain query values.
#[derive(Debug)]
pub struct ListingQuery<K> {
    pub query: ODataQuery,
    pairs: Vec<(String, String)>,
    keys: PhantomData<K>,
}

impl<K> ListingQuery<K> {
    /// The value of a plain key; the extractor has already rejected repeated keys.
    #[must_use]
    pub fn value(&self, key: &str) -> Option<&str> {
        self.pairs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// `$` options the platform extractor binds but no listing here supports: the order is fixed,
/// nothing is filterable and items have no field projection.
fn unsupported_bound_option(key: &str) -> Option<&'static str> {
    match key {
        "$filter" => Some("filtering is not supported by this listing"),
        "$orderby" => Some("the order of this listing is fixed"),
        "$select" => Some("this listing has no field projection"),
        _ => None,
    }
}

/// Describe one rejected `$` key outside the accepted set.
fn unsupported_system_option_detail(key: &str) -> String {
    let canonical = key.to_ascii_lowercase();
    if ACCEPTED_SYSTEM_QUERY_OPTIONS.contains(&canonical.as_str()) {
        return format!("`{key}` binds only as `{canonical}`");
    }
    match canonical.as_str() {
        "$skip" => "this listing pages by cursor, not by offset: send the previous page's \
                    `next_cursor` as `$skiptoken` (alias `cursor`)"
            .to_owned(),
        "$count" => "a page carries no total".to_owned(),
        _ => format!("unsupported query option `{key}`"),
    }
}

/// The `$` name of the page parameter a key spells (`limit`/`$top`, `cursor`/`$skiptoken`).
fn page_parameter(key: &str) -> Option<&'static str> {
    match key {
        "limit" | "$top" => Some("$top"),
        "cursor" | "$skiptoken" => Some("$skiptoken"),
        _ => None,
    }
}

/// The violation of one key given `occurrences` times, if any.
fn violation_of<K: ListingKeys>(key: &str, occurrences: usize) -> Option<(&'static str, String)> {
    if key.starts_with('$') {
        if let Some(detail) = unsupported_bound_option(key) {
            return Some((UNSUPPORTED_QUERY_PARAM, detail.to_owned()));
        }
        if ACCEPTED_SYSTEM_QUERY_OPTIONS.contains(&key) {
            return None;
        }
        return Some((
            UNSUPPORTED_QUERY_PARAM,
            unsupported_system_option_detail(key),
        ));
    }
    if !K::KEYS.contains(&key) {
        return Some((
            UNSUPPORTED_QUERY_PARAM,
            format!(
                "`{key}` is not a parameter of this listing; it takes {} (`$top` and `$skiptoken` \
                 are synonyms of `limit` and `cursor`)",
                K::KEYS.join(", ")
            ),
        ));
    }
    (occurrences > 1).then(|| {
        (
            INVALID_QUERY_PARAMS,
            format!("`{key}` is given more than once"),
        )
    })
}

fn reject_bad_keys<K: ListingKeys>(pairs: &[(String, String)]) -> Result<(), CanonicalError> {
    let mut seen: Vec<&str> = Vec::new();
    let mut violations: Vec<(&str, &'static str, String)> = Vec::new();
    for key in pairs.iter().map(|(key, _)| key.as_str()) {
        // A page parameter is one parameter whichever spelling is sent.
        let identity = page_parameter(key).unwrap_or(key);
        if seen.contains(&identity) {
            continue;
        }
        seen.push(identity);
        let spellings: Vec<&str> = pairs
            .iter()
            .map(|(k, _)| k.as_str())
            .filter(|k| page_parameter(k).unwrap_or(k) == identity)
            .collect();
        if let Some(canonical) = page_parameter(key) {
            if spellings.len() > 1 {
                let mut sent: Vec<String> = Vec::new();
                for spelling in spellings {
                    let quoted = format!("`{spelling}`");
                    if !sent.contains(&quoted) {
                        sent.push(quoted);
                    }
                }
                violations.push((
                    canonical,
                    INVALID_QUERY_PARAMS,
                    format!(
                        "`{canonical}` is given more than once (sent as {})",
                        sent.join(", ")
                    ),
                ));
            }
        } else if let Some((reason, detail)) = violation_of::<K>(key, spellings.len()) {
            violations.push((key, reason, detail));
        }
    }

    let mut violations = violations.into_iter();
    let Some((key, reason, detail)) = violations.next() else {
        return Ok(());
    };
    let mut error = OdataError::invalid_argument().with_field_violation(key, detail, reason);
    for (key, reason, detail) in violations {
        error = error.with_field_violation(key, detail, reason);
    }
    Err(error.create())
}

impl<K, S> FromRequestParts<S> for ListingQuery<K>
where
    K: ListingKeys,
    S: Send + Sync,
{
    type Rejection = CanonicalError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let Query(pairs) = Query::<Vec<(String, String)>>::from_request_parts(parts, state).await?;
        reject_bad_keys::<K>(&pairs)?;
        let query = extract_odata_query(parts, state).await?;
        Ok(Self {
            query,
            pairs,
            keys: PhantomData,
        })
    }
}
