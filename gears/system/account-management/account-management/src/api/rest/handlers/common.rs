//! Cross-handler helpers shared by the AM REST handler families.

use std::collections::HashMap;

use toolkit_odata::ODataQuery;

use crate::domain::error::DomainError;

/// Clamp the `OData` `$top` against the per-endpoint deployment cap.
/// Repos already enforce an absolute ceiling (200), but a deployment
/// that has dropped `listing.max_top` below it would otherwise be
/// bypassed — clamp here so the service signature stays a thin
/// `(scope, target, &ODataQuery)` forward.
pub(super) fn clamp_listing_top(mut query: ODataQuery, max_top: u32) -> ODataQuery {
    let cap = u64::from(max_top);
    query.limit = Some(query.limit.map_or(cap, |requested| requested.min(cap)));
    query
}

/// Reject any query parameter that does not start with `$`.
///
/// AM list endpoints use `OData` as the single filter / ordering /
/// pagination surface (`$filter`, `$orderby`, `$select`, `$top`,
/// `$skiptoken`). Without this guard, Axum silently drops
/// query keys that no extractor claimed — a caller writing in a
/// generic-REST convention like `?status=approved` would receive
/// HTTP 200 with the **unfiltered** result set and assume the filter
/// applied. That is a documented contract-drift surface (the e2e
/// pin `test_conversion_list_plain_status_param_silently_ignored`
/// in vhp-core asserts the 400 shape).
///
/// Mapping the violation to [`DomainError::Validation`] surfaces a
/// canonical HTTP 400 with the `$filter` hint embedded in `detail`
/// so clients see the canonical contract without parsing the
/// envelope.
///
/// `$`-prefixed keys are intentionally out of scope: the `OData`
/// extractor binds the options in
/// `toolkit::api::odata::ACCEPTED_SYSTEM_QUERY_OPTIONS` and rejects
/// every other `$` key (`$skip`, `$count`, `$filtre`) with its own
/// `400`. This check is the seam for non-`OData` accidents only.
pub(super) fn reject_non_odata_params(query: &HashMap<String, String>) -> Result<(), DomainError> {
    if let Some(unknown) = query.keys().find(|k| !k.starts_with('$')) {
        return Err(DomainError::Validation {
            detail: format!(
                "unrecognized query parameter `{unknown}`; AM list endpoints \
                 accept OData parameters only (e.g. `$filter=status eq 'approved'`)"
            ),
        });
    }
    Ok(())
}

#[cfg(test)]
#[path = "common_tests.rs"]
mod tests;
