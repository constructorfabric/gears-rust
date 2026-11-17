use oagw_sdk::field;
use toolkit_canonical_errors::builder::{NeedsFieldViolation, ResourceOptional};
use toolkit_canonical_errors::{Problem, ResourceErrorBuilder};
use uuid::Uuid;

use crate::api::rest::error::domain_error_to_problem;
use crate::domain::error::DomainError;
use crate::domain::gts_helpers;
use crate::domain::model::ListQuery;

/// Parse a GTS identifier, verifying that its schema prefix matches
/// `expected_schema` (e.g. `UPSTREAM_SCHEMA`). Returns a validation
/// `Problem` (with `instance` pre-populated from the supplied request
/// URI) if the prefix does not match.
pub fn parse_gts_id(
    gts_str: &str,
    expected_schema: &str,
    instance: &str,
) -> Result<Uuid, Box<Problem>> {
    let (schema, uuid) = gts_helpers::parse_resource_gts(gts_str)
        .map_err(|e| domain_error_to_problem(e, instance))?;
    let expected_prefix = expected_schema.trim_end_matches('~');
    let actual_prefix = schema.trim_end_matches('~');
    if actual_prefix != expected_prefix {
        let err = DomainError::Validation {
            field: "gts_id",
            reason: field::INVALID_GTS_SCHEMA,
            detail: format!("expected GTS schema '{expected_schema}' but got '{schema}'"),
            instance: instance.to_string(),
        };
        return Err(Box::new(domain_error_to_problem(err, instance)));
    }
    Ok(uuid)
}

/// Refuse a list request whose query string has a parameter other than
/// `T::KEYS` (`$filter`, `$orderby`, a misspelt `limt`): without this the
/// parameter would be dropped and the caller would get an unfiltered 200.
fn reject_unknown_query_params<T: ListQueryParams>(
    raw_query: &str,
    instance: &str,
) -> Result<(), Box<Problem>> {
    let allowed = T::KEYS;
    // One violation per key, named by the key, deduplicated in wire order:
    // the platform's `OData` extractor reports them the same way. The caller
    // chooses the keys, so at most `MAX_REPORTED_QUERY_PARAMS` are reported,
    // each cut to `MAX_REPORTED_KEY_BYTES`.
    let mut offending: Vec<String> = Vec::new();
    for (key, _) in form_urlencoded::parse(raw_query.as_bytes()) {
        if allowed.contains(&key.as_ref()) {
            continue;
        }
        let key = truncate_utf8(&key, MAX_REPORTED_KEY_BYTES);
        if !offending.iter().any(|k| k == key) {
            offending.push(key.to_owned());
            if offending.len() == MAX_REPORTED_QUERY_PARAMS {
                break;
            }
        }
    }
    let mut offenders = offending.iter();
    let Some(first) = offenders.next() else {
        return Ok(());
    };
    let supported = allowed.join(", ");
    let detail =
        |key: &str| format!("query parameter '{key}' is not supported; supported: {supported}");
    let mut error = T::invalid_argument().with_field_violation(
        first,
        detail(first),
        field::UNSUPPORTED_QUERY_PARAM,
    );
    for key in offenders {
        error = error.with_field_violation(key, detail(key), field::UNSUPPORTED_QUERY_PARAM);
    }
    let mut problem = Problem::from(error.create());
    problem.instance = Some(instance.to_owned());
    Err(Box::new(problem))
}

/// A list endpoint's query parameters, and the query keys they read.
pub trait ListQueryParams: serde::de::DeserializeOwned {
    /// The keys `Self` reads: any other key is refused.
    const KEYS: &'static [&'static str];

    /// A 400 about the query, scoped to the listed resource (its
    /// `context.resource_type`).
    fn invalid_argument() -> ResourceErrorBuilder<ResourceOptional, NeedsFieldViolation>;
}

/// Parse a list request's query string as `T`, after refusing keys other
/// than `T::KEYS` ([`reject_unknown_query_params`]). Unsupported keys are
/// reported first, and a value `T` cannot parse is a problem response too,
/// not the framework's plain-text rejection.
pub fn parse_list_query<T: ListQueryParams>(
    uri: &http::Uri,
    instance: &str,
) -> Result<T, Box<Problem>> {
    reject_unknown_query_params::<T>(uri.query().unwrap_or(""), instance)?;
    let axum::extract::Query(query) =
        axum::extract::Query::<T>::try_from_uri(uri).map_err(|e| {
            let error = T::invalid_argument()
                .with_field_violation("query", e.body_text(), field::VALIDATION)
                .create();
            let mut problem = Problem::from(error);
            problem.instance = Some(instance.to_owned());
            Box::new(problem)
        })?;
    Ok(query)
}

/// How many unsupported list parameters a 400 names.
const MAX_REPORTED_QUERY_PARAMS: usize = 16;

/// How much of an unsupported parameter's name a 400 echoes.
const MAX_REPORTED_KEY_BYTES: usize = 64;

/// `s` cut to at most `max` bytes, on a character boundary.
fn truncate_utf8(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Query parameters of `GET /oagw/v1/upstreams`: pagination only.
#[derive(Debug, serde::Deserialize)]
#[cfg_attr(test, derive(serde::Serialize, PartialEq))]
pub struct PaginationQuery {
    #[serde(default = "default_top")]
    pub limit: u32,
    #[serde(default)]
    pub offset: u32,
}

fn default_top() -> u32 {
    50
}

impl ListQueryParams for PaginationQuery {
    const KEYS: &'static [&'static str] = &["limit", "offset"];

    fn invalid_argument() -> ResourceErrorBuilder<ResourceOptional, NeedsFieldViolation> {
        crate::api::rest::error::OagwUpstreamError::invalid_argument()
    }
}

impl PaginationQuery {
    pub fn to_list_query(&self) -> ListQuery {
        ListQuery {
            top: self.limit.min(100),
            skip: self.offset,
        }
    }
}

#[cfg(test)]
#[path = "extractors_tests.rs"]
mod extractors_tests;
