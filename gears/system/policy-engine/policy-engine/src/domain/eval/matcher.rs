//! Which documents apply to a request.
//!
//! A document applies when some pattern of its `resource_types` matches the
//! request's GTS type identifier ([`GtsId::matches_pattern`]; a concrete type
//! id is a pattern without a wildcard) and its `actions` is empty or names the
//! request's action.

use gts::{GtsId, GtsIdPattern};

/// Whether a document with `resource_types` and `actions` applies to a
/// request for `action` on `type_id`.
#[must_use]
pub fn document_applies(
    resource_types: &[GtsIdPattern],
    actions: &[String],
    type_id: &GtsId,
    action: &str,
) -> bool {
    (actions.is_empty() || actions.iter().any(|a| a == action))
        && resource_types
            .iter()
            .any(|pattern| type_id.matches_pattern(pattern))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "matcher_tests.rs"]
mod matcher_tests;
