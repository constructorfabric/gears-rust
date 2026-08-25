//! Unit tests for [`super::clamp_listing_top`] and
//! [`super::reject_non_odata_params`]. Live alongside the helpers so
//! a future change to the policy cannot drift silently — every AM
//! listing handler clamps `$top` and rejects non-`OData` query keys
//! through the same seam.

use std::collections::HashMap;

use super::{clamp_listing_top, reject_non_odata_params};
use crate::domain::error::DomainError;
use toolkit_odata::ODataQuery;

#[test]
fn clamp_listing_top_defaults_unset_limit_to_operator_cap() {
    // A caller that omits `$top` should inherit the operator-tuned
    // cap rather than the repo-level absolute ceiling. Without this,
    // a deployment with `listing.max_top = 25` would still issue an
    // unbounded query to the repo and rely on the repo-level
    // `*_LISTING_LIMIT_CFG.max = 200` -- bypassing the per-deployment
    // policy.
    let query = ODataQuery::new();
    let clamped = clamp_listing_top(query, 25);
    assert_eq!(clamped.limit, Some(25));
}

#[test]
fn clamp_listing_top_caps_oversized_caller_limit_to_operator_cap() {
    let query = ODataQuery::new().with_limit(500);
    let clamped = clamp_listing_top(query, 25);
    assert_eq!(clamped.limit, Some(25));
}

#[test]
fn clamp_listing_top_preserves_smaller_caller_limit() {
    // A caller-supplied `$top` BELOW the cap is preserved verbatim --
    // the clamp is an upper bound, not a forced default.
    let query = ODataQuery::new().with_limit(10);
    let clamped = clamp_listing_top(query, 25);
    assert_eq!(clamped.limit, Some(10));
}

#[test]
fn clamp_listing_top_with_max_cap_allows_repo_absolute_ceiling() {
    // When the operator cap matches the repo's absolute ceiling
    // (`*_LISTING_LIMIT_CFG.max = 200`) the clamp degenerates into a
    // no-op for in-range caller values -- preserve the documented
    // default behaviour.
    let query = ODataQuery::new().with_limit(50);
    let clamped = clamp_listing_top(query, 200);
    assert_eq!(clamped.limit, Some(50));
}

#[test]
fn reject_non_odata_params_passes_empty_query() {
    let q: HashMap<String, String> = HashMap::new();
    reject_non_odata_params(&q).expect("empty query must pass");
}

#[test]
fn reject_non_odata_params_passes_odata_only_keys() {
    // The full set of `OData` keys AM listing endpoints accept must
    // pass the gate; a regression that accidentally narrowed the
    // allow-shape would trip here.
    //
    // This gate polices the non-`$` namespace only, so it is not the
    // place that refuses unsupported system query options: `$skip` and
    // `$count` pass here and are rejected downstream by the extractor
    // (`toolkit::api::odata::ACCEPTED_SYSTEM_QUERY_OPTIONS`).
    let mut q = HashMap::new();
    q.insert("$filter".to_owned(), "status eq 'approved'".to_owned());
    q.insert("$orderby".to_owned(), "created_at desc".to_owned());
    q.insert("$top".to_owned(), "10".to_owned());
    q.insert("$skiptoken".to_owned(), "opaque-cursor".to_owned());
    q.insert("$select".to_owned(), "id,status".to_owned());
    reject_non_odata_params(&q).expect("OData-only query must pass");
}

#[test]
fn reject_non_odata_params_rejects_plain_status_with_filter_hint() {
    // Exact CL8 shape pinned by
    // `test_conversion_list_plain_status_param_silently_ignored` in
    // the vhp-core e2e suite: `?status=approved` on a conversion-list
    // endpoint must surface as 400 `Validation` with the `$filter`
    // hint, not silently ignored as it was pre-fix.
    let mut q = HashMap::new();
    q.insert("status".to_owned(), "approved".to_owned());
    let err = reject_non_odata_params(&q).expect_err("plain `status` must reject");
    let DomainError::Validation { detail } = err else {
        panic!("expected DomainError::Validation, got {err:?}");
    };
    assert!(
        detail.contains("status"),
        "detail must name the offending parameter: {detail}"
    );
    assert!(
        detail.contains("$filter"),
        "detail must hint at the OData replacement: {detail}"
    );
}

#[test]
fn reject_non_odata_params_rejects_any_non_dollar_key() {
    // Generic gate — the rejection is not `status`-specific. A typo
    // like `?filter=...` (missing the leading `$`) lands here exactly
    // as `?status=...` would, with the same hint pointing at the
    // canonical contract.
    let mut q = HashMap::new();
    q.insert("filter".to_owned(), "status eq 'approved'".to_owned());
    let err = reject_non_odata_params(&q).expect_err("non-`$` `filter` must reject");
    let DomainError::Validation { detail } = err else {
        panic!("expected DomainError::Validation, got {err:?}");
    };
    assert!(detail.contains("filter"));
    assert!(detail.contains("$filter"));
}

#[test]
fn reject_non_odata_params_rejects_when_mixed_with_odata_keys() {
    // Defence against the "but I also sent `$filter`" false-confidence
    // case: a caller mixing a plain key with a real `OData` filter
    // still gets a 400. Without this, partial silent-drop would
    // mask the contradiction (which `$filter` wins? — undefined).
    let mut q = HashMap::new();
    q.insert("$filter".to_owned(), "status eq 'pending'".to_owned());
    q.insert("status".to_owned(), "approved".to_owned());
    let err = reject_non_odata_params(&q).expect_err("mixed query must reject on the plain key");
    assert!(matches!(err, DomainError::Validation { .. }));
}
