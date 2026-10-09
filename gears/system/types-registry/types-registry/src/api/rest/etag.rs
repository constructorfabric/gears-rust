//! If-None-Match parsing (RFC 9110 §13.1.2), sharing exact/batch tag bytes via
//! [`crate::api::encoding::entity_tag`]. Unreadable conditions are refused.

use axum::http::{HeaderMap, header};
use toolkit_canonical_errors::CanonicalError;
use types_registry_sdk::field;

use super::error::malformed_condition;
use crate::api::encoding::entity_tag::token;
pub use crate::api::encoding::entity_tag::{entity_tag, item_condition};
use crate::domain::validator::IfNoneMatch;

/// The `If-None-Match` header: `*` or a list of entity-tags (RFC 9110 §13.1.2),
/// across however many header lines. Empty list elements are ignored (§5.6.1.2);
/// a list with no element at all, or `*` beside a tag, is refused.
pub fn header_condition(headers: &HeaderMap) -> Result<Option<IfNoneMatch>, CanonicalError> {
    let field = field::IF_NONE_MATCH_HEADER;
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
