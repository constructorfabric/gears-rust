//! HTTP entity-tag framing of the domain validator (RFC 9110 §8.8.3). The exact
//! read's `ETag` and a batch result's `etag` carry identical bytes (SPEC §8.5).
//!
//! A condition that cannot be read is refused rather than dropped: a caller that
//! sent one believes the read is conditional, and answering unconditionally
//! would be answering a different question.

use axum::http::{HeaderMap, header};
use toolkit_canonical_errors::CanonicalError;

use super::error::{malformed_condition, validator_too_long, violation_field};
use crate::domain::registry_service::MAX_KEY_LEN;
use crate::domain::validator::{IfNoneMatch, Validator};

/// A strong entity-tag: the validator, quoted.
pub fn entity_tag(etag: Validator) -> String {
    format!("\"{}\"", etag.encode())
}

/// The opaque part of one entity-tag, `W/` dropped as the weak comparison
/// `If-None-Match` uses requires. `None` when `tag` is not an entity-tag: the
/// opaque part is `etagc` (RFC 9110 §8.8.3), visible ASCII but `"`; `obs-text`
/// is refused with the rest of non-ASCII.
fn opaque(tag: &str) -> Option<&str> {
    let tag = tag.strip_prefix("W/").unwrap_or(tag);
    let inner = tag.strip_prefix('"')?.strip_suffix('"')?;
    inner
        .bytes()
        .all(|byte| byte == b'!' || (b'#'..=b'~').contains(&byte))
        .then_some(inner)
}

/// One entity-tag, bounded like a key, as a validator token.
fn token(tag: &str, field: &'static str) -> Result<String, CanonicalError> {
    let inner = opaque(tag)
        .ok_or_else(|| malformed_condition(field, "each value must be a quoted entity-tag"))?;
    if inner.len() > MAX_KEY_LEN {
        return Err(validator_too_long(field, inner.len()));
    }
    Ok(inner.to_owned())
}

/// A batch item's `if_none_match`: exactly one entity-tag from an earlier read of
/// that key. `*` is not one — "unchanged if it exists" is not a question a batch
/// item asks.
pub fn item_condition(tag: &str) -> Result<IfNoneMatch, CanonicalError> {
    let tag = tag.trim();
    if tag == "*" {
        return Err(malformed_condition(
            violation_field::IF_NONE_MATCH_ITEM,
            "if_none_match takes the etag of an earlier read, and `*` is not one",
        ));
    }
    Ok(IfNoneMatch::Validators(vec![token(
        tag,
        violation_field::IF_NONE_MATCH_ITEM,
    )?]))
}

/// The `If-None-Match` header: `*` or a list of entity-tags (RFC 9110 §13.1.2),
/// across however many header lines. Empty list elements are ignored (§5.6.1.2);
/// a list with no element at all, or `*` beside a tag, is refused.
pub fn header_condition(headers: &HeaderMap) -> Result<Option<IfNoneMatch>, CanonicalError> {
    let field = violation_field::IF_NONE_MATCH;
    let mut elements = Vec::new();
    for value in headers.get_all(header::IF_NONE_MATCH) {
        let value = value
            .to_str()
            .map_err(|_| malformed_condition(field, "the header must be visible ASCII"))?;
        elements.extend(
            split_list(value)
                .into_iter()
                .map(str::trim)
                .filter(|element| !element.is_empty()),
        );
    }
    if elements.is_empty() {
        return if headers.contains_key(header::IF_NONE_MATCH) {
            Err(malformed_condition(field, "the header names no entity-tag"))
        } else {
            Ok(None)
        };
    }
    if elements.contains(&"*") {
        return if elements.len() == 1 {
            Ok(Some(IfNoneMatch::Any))
        } else {
            Err(malformed_condition(
                field,
                "`*` stands alone; it cannot be listed with entity-tags",
            ))
        };
    }
    let tags = elements
        .into_iter()
        .map(|tag| token(tag, field))
        .collect::<Result<_, _>>()?;
    Ok(Some(IfNoneMatch::Validators(tags)))
}

/// Split a list on the commas outside quotes: an opaque tag may hold a comma.
fn split_list(value: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut quoted = false;
    let mut start = 0;
    for (at, byte) in value.bytes().enumerate() {
        match byte {
            b'"' => quoted = !quoted,
            b',' if !quoted => {
                parts.push(&value[start..at]);
                start = at + 1;
            }
            _ => {}
        }
    }
    parts.push(&value[start..]);
    parts
}

#[cfg(test)]
#[path = "etag_tests.rs"]
mod tests;
