//! Unit tests for [`super::clamp_listing_top`] and
//! [`super::reject_non_odata_params`]. Live alongside the helpers so
//! a future change to the policy cannot drift silently — every AM
//! listing handler clamps `$top` and rejects non-`OData` query keys
//! through the same seam.

use std::collections::HashMap;

use super::{
    bind_cursor_to_children_mode, clamp_listing_top, parse_recursive_flag, reject_non_odata_params,
};
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

#[test]
fn parse_recursive_flag_absent_is_false() {
    let q: HashMap<String, String> = HashMap::new();
    assert!(!parse_recursive_flag(&q).expect("absent"));
}

#[test]
fn parse_recursive_flag_accepts_exact_literals() {
    let mut q = HashMap::new();
    q.insert("recursive".to_owned(), "true".to_owned());
    assert!(parse_recursive_flag(&q).expect("true"));
    q.insert("recursive".to_owned(), "false".to_owned());
    assert!(!parse_recursive_flag(&q).expect("false"));
}

#[test]
fn parse_recursive_flag_rejects_anything_else_as_validation() {
    for bad in ["True", "1", "yes", "", "TRUE"] {
        let mut q = HashMap::new();
        q.insert("recursive".to_owned(), bad.to_owned());
        let err = parse_recursive_flag(&q).expect_err(bad);
        assert_eq!(err.code(), "validation", "value `{bad}` must be a 400");
    }
}

#[test]
fn parse_recursive_flag_ignores_other_keys() {
    let mut q = HashMap::new();
    q.insert("limit".to_owned(), "10".to_owned());
    q.insert("$filter".to_owned(), "name eq 'x'".to_owned());
    assert!(!parse_recursive_flag(&q).expect("other keys are not this parser's business"));
}

#[test]
fn bind_cursor_to_children_mode_separates_the_two_modes() {
    // No `$filter`: the extractor leaves `filter_hash` unset.
    let direct = bind_cursor_to_children_mode(ODataQuery::new(), false).expect("direct");
    let recursive = bind_cursor_to_children_mode(ODataQuery::new(), true).expect("recursive");
    assert_eq!(direct.filter_hash.as_deref(), Some("children"));
    assert_eq!(recursive.filter_hash.as_deref(), Some("recursive:"));

    // With `$filter`: direct keeps the extractor's hash verbatim.
    let filtered = ODataQuery::new().with_filter_hash("abc123".to_owned());
    let direct = bind_cursor_to_children_mode(filtered.clone(), false).expect("direct");
    let recursive = bind_cursor_to_children_mode(filtered, true).expect("recursive");
    assert_eq!(direct.filter_hash.as_deref(), Some("abc123"));
    assert_eq!(recursive.filter_hash.as_deref(), Some("recursive:abc123"));
    assert_ne!(direct.filter_hash, recursive.filter_hash);
}

fn cursor_with_fingerprint(f: Option<&str>) -> toolkit_odata::CursorV1 {
    toolkit_odata::CursorV1 {
        k: vec![
            "2026-01-01T00:00:00Z".to_owned(),
            "00000000-0000-0000-0000-000000000001".to_owned(),
        ],
        o: toolkit_odata::SortDir::Asc,
        s: "+created_at,+id".to_owned(),
        f: f.map(str::to_owned),
        d: "fwd".to_owned(),
    }
}

#[test]
fn bind_cursor_to_children_mode_rejects_a_legacy_cursor_in_recursive_mode() {
    // A direct-listing cursor minted before mode binding has no `f`.
    let legacy = ODataQuery::new().with_cursor(cursor_with_fingerprint(None));
    let err = bind_cursor_to_children_mode(legacy, true).expect_err("recursive must reject");
    assert_eq!(err.code(), "validation");
    assert!(err.to_string().contains("FILTER_MISMATCH"), "{err}");
}

#[test]
fn bind_cursor_to_children_mode_keeps_legacy_cursors_working_in_direct_mode() {
    let legacy = ODataQuery::new().with_cursor(cursor_with_fingerprint(None));
    let bound = bind_cursor_to_children_mode(legacy, false).expect("direct keeps accepting");
    assert_eq!(bound.filter_hash.as_deref(), Some("children"));
}

#[test]
fn bind_cursor_to_children_mode_passes_a_fingerprinted_cursor_to_pagination() {
    // A fingerprinted cursor is left to the pagination check, which
    // compares it with the bound hash.
    let bound = ODataQuery::new().with_cursor(cursor_with_fingerprint(Some("recursive:")));
    assert!(bind_cursor_to_children_mode(bound, true).is_ok());
}
