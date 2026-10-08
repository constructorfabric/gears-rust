//! REST's entity-tag representation of the domain validator token (RFC 9110 §8.8.3).
//! Reject unreadable conditions: answering unconditionally would change the caller’s request.

use toolkit_canonical_errors::CanonicalError;
use types_registry_sdk::field;

use crate::api::error::{malformed_condition, validator_too_long};
use crate::domain::registry_service::MAX_KEY_LEN;
use crate::domain::validator::{IfNoneMatch, Validator};

/// A strong entity-tag: the validator, quoted.
pub fn entity_tag(etag: Validator) -> String {
    format!("\"{}\"", etag.encode())
}

/// Strip W/ for weak If-None-Match comparison; require quoted ASCII etagc without quotes
/// (RFC 9110 §8.8.3). Non-ASCII/obs-text is refused.
fn opaque(tag: &str) -> Option<&str> {
    let tag = tag.strip_prefix("W/").unwrap_or(tag);
    let inner = tag.strip_prefix('"')?.strip_suffix('"')?;
    inner
        .bytes()
        .all(|byte| byte == b'!' || (b'#'..=b'~').contains(&byte))
        .then_some(inner)
}

/// One entity-tag, bounded like a key, as a validator token.
pub fn token(tag: &str, field: &'static str) -> Result<String, CanonicalError> {
    let inner = opaque(tag)
        .ok_or_else(|| malformed_condition(field, "each value must be a quoted entity-tag"))?;
    if inner.len() > MAX_KEY_LEN {
        return Err(validator_too_long(field, inner.len()));
    }
    Ok(inner.to_owned())
}

/// One previous entity-tag for a batch key; wildcard existence checks are not supported.
pub fn item_condition(tag: &str) -> Result<IfNoneMatch, CanonicalError> {
    let tag = tag.trim();
    if tag == "*" {
        return Err(malformed_condition(
            field::IF_NONE_MATCH_FIELD,
            "if_none_match takes the etag of an earlier read, and `*` is not one",
        ));
    }
    Ok(IfNoneMatch::Validators(vec![token(
        tag,
        field::IF_NONE_MATCH_FIELD,
    )?]))
}
