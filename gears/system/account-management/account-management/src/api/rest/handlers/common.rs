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

/// Parse the `recursive` flag of `GET /tenants/{tenant_id}/children`.
///
/// Absent → `false`. Exactly the lowercase literals `true` / `false`
/// are accepted; anything else is a `validation` error so a typo
/// (`recursive=True`, `recursive=1`) never silently degrades to the
/// non-recursive listing. Other keys are not inspected: the `OData`
/// extractor owns the `$` namespace and `limit` / `cursor`, and
/// `/children` has always ignored unrecognised non-`$` keys.
pub(super) fn parse_recursive_flag(query: &HashMap<String, String>) -> Result<bool, DomainError> {
    match query.get("recursive").map(String::as_str) {
        None | Some("false") => Ok(false),
        Some("true") => Ok(true),
        Some(other) => Err(DomainError::Validation {
            detail: format!(
                "invalid `recursive` value `{other}`; expected exactly `true` or `false`"
            ),
        }),
    }
}

/// Bind the cursor fingerprint of a `/children` request to its mode.
///
/// The keyset cursor carries `f` = the query's `filter_hash`, and
/// pagination rejects a cursor whose `f` differs from the follow-up
/// request's hash (`400 FILTER_MISMATCH`) — but only when both are
/// present, and the extractor sets the hash only when `$filter` is.
/// Both modes share the effective order `(created_at ASC, id ASC)`, so
/// a cursor minted in one mode and replayed in the other would
/// silently skip rows of the other set. Folding the mode into the hash
/// turns that into the same `FILTER_MISMATCH` a changed `$filter`
/// produces:
///
/// * recursive: `recursive:<filter hash or empty>`;
/// * direct, with `$filter`: the unchanged filter hash, so direct-mode
///   cursors minted before this change keep working;
/// * direct, without `$filter`: the constant `children`.
///
/// A cursor that carries no fingerprint at all can only come from a
/// direct listing minted before the binding existed. The pagination
/// check skips a missing `f`, so in recursive mode such a cursor is
/// rejected here instead; the direct listing keeps accepting it.
///
/// # Errors
///
/// `Validation` (`400`, `FILTER_MISMATCH`) for a fingerprint-less
/// cursor replayed with `recursive=true`.
pub(super) fn bind_cursor_to_children_mode(
    mut query: ODataQuery,
    recursive: bool,
) -> Result<ODataQuery, DomainError> {
    if recursive && query.cursor.as_ref().is_some_and(|c| c.f.is_none()) {
        return Err(DomainError::Validation {
            detail: "list_descendants query rejected: FILTER_MISMATCH (the cursor was \
                     issued by a direct listing that predates mode binding)"
                .to_owned(),
        });
    }
    let filter_hash = query.filter_hash.take();
    query.filter_hash = Some(match (recursive, filter_hash) {
        (true, hash) => format!("recursive:{}", hash.as_deref().unwrap_or_default()),
        (false, Some(hash)) => hash,
        (false, None) => "children".to_owned(),
    });
    Ok(query)
}

#[cfg(test)]
#[path = "common_tests.rs"]
mod tests;
