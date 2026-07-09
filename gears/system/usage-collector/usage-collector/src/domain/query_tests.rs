//! Unit tests for [`require_bounded_time_window`] — the gateway guard that
//! rejects raw / aggregated queries whose `$filter` does not pin a bounded
//! `created_at` window (a lower **and** an upper bound as top-level
//! conjuncts), preventing an unbounded full-table scan / aggregation.

use toolkit_odata::ODataQuery;
use toolkit_security::{AccessScope, ScopeConstraint, ScopeFilter, pep_properties};
use usage_collector_sdk::{UsageCollectorError, ValidationReason};
use uuid::Uuid;

use super::{compose_query_with_scope, require_bounded_time_window};

/// Build an [`ODataQuery`] whose `$filter` is the parsed `filter` string.
fn query_with_filter(filter: &str) -> ODataQuery {
    let expr = toolkit_odata::parse_filter_string(filter)
        .expect("test filter parses")
        .into_expr();
    ODataQuery::from(Some(expr))
}

/// Assert the error is the canonical missing-window rejection.
fn assert_missing_window(err: UsageCollectorError) {
    match err {
        UsageCollectorError::InvalidArgument { field, reason, .. } => {
            assert_eq!(field, "$filter", "window violation attributes to $filter");
            assert_eq!(reason, ValidationReason::MissingTimeWindow);
        }
        other => panic!("expected InvalidArgument/MissingTimeWindow, got {other:?}"),
    }
}

#[test]
fn lower_and_upper_bound_is_accepted() {
    let q = query_with_filter(
        "created_at ge 2026-01-01T00:00:00Z and created_at lt 2026-02-01T00:00:00Z",
    );
    require_bounded_time_window(&q).expect("a bounded window is accepted");
}

#[test]
fn gt_and_le_bounds_are_accepted() {
    let q = query_with_filter(
        "created_at gt 2026-01-01T00:00:00Z and created_at le 2026-02-01T00:00:00Z",
    );
    require_bounded_time_window(&q).expect("gt/le also bound the window");
}

#[test]
fn empty_filter_is_rejected() {
    let err =
        require_bounded_time_window(&ODataQuery::new()).expect_err("no filter means no window");
    assert_missing_window(err);
}

#[test]
fn lower_bound_only_is_rejected() {
    let q = query_with_filter("created_at ge 2026-01-01T00:00:00Z");
    let err = require_bounded_time_window(&q).expect_err("an open-ended upper edge is unbounded");
    assert_missing_window(err);
}

#[test]
fn upper_bound_only_is_rejected() {
    let q = query_with_filter("created_at lt 2026-02-01T00:00:00Z");
    let err = require_bounded_time_window(&q).expect_err("an open-ended lower edge is unbounded");
    assert_missing_window(err);
}

#[test]
fn equality_alone_does_not_bound_the_window() {
    // A point predicate is neither a lower (`ge`/`gt`) nor an upper
    // (`le`/`lt`) bound, so it does not satisfy the bounded-window contract.
    let q = query_with_filter("created_at eq 2026-01-01T00:00:00Z");
    let err = require_bounded_time_window(&q).expect_err("eq is not a range bound");
    assert_missing_window(err);
}

#[test]
fn bounds_disjoined_under_or_are_not_top_level() {
    // The window predicates are OR-ed with another clause, so neither is an
    // effective conjunctive bound — rows outside the window can still match.
    let q = query_with_filter(
        "(created_at ge 2026-01-01T00:00:00Z and created_at lt 2026-02-01T00:00:00Z) \
         or status eq 'active'",
    );
    let err = require_bounded_time_window(&q).expect_err("OR breaks the conjunctive bound");
    assert_missing_window(err);
}

#[test]
fn negated_window_is_rejected() {
    let q = query_with_filter(
        "not (created_at ge 2026-01-01T00:00:00Z and created_at lt 2026-02-01T00:00:00Z)",
    );
    let err = require_bounded_time_window(&q).expect_err("NOT inverts the window");
    assert_missing_window(err);
}

#[test]
fn bounds_among_other_top_level_conjuncts_are_accepted() {
    let q = query_with_filter(
        "status eq 'active' and created_at ge 2026-01-01T00:00:00Z \
         and resource_id eq 'r1' and created_at lt 2026-02-01T00:00:00Z",
    );
    require_bounded_time_window(&q).expect("window bounds among other conjuncts still count");
}

// ---------------------------------------------------------------------------
// compose_query_with_scope — AND-merges PDP scope but MUST keep filter_hash
// pinned to the user `$filter` (keyset-cursor stability across pages).
// ---------------------------------------------------------------------------

/// A user query whose `filter_hash` mirrors the gateway: it is the hash of the
/// USER `$filter`, not of anything the service later AND-merges in.
fn user_query_with_gateway_hash(filter: &str) -> ODataQuery {
    let mut q = query_with_filter(filter);
    q.filter_hash = toolkit_odata::short_filter_hash(q.filter());
    q
}

#[test]
fn compose_preserves_user_filter_hash_when_scope_narrows() {
    // Regression for the keyset-pagination FILTER_MISMATCH 400: re-hashing the
    // AND-merged filter into `filter_hash` embeds a hash in the next_cursor
    // that the gateway's hash(user $filter) can never match on the follow-up
    // page. `filter_hash` must stay the USER-filter hash.
    let user = user_query_with_gateway_hash(
        "created_at ge 2026-01-01T00:00:00Z and created_at lt 2026-02-01T00:00:00Z",
    );
    let user_hash = user.filter_hash.clone();
    assert!(
        user_hash.is_some(),
        "precondition: gateway populated filter_hash"
    );

    let scope = AccessScope::single(ScopeConstraint::new(vec![ScopeFilter::in_uuids(
        pep_properties::OWNER_TENANT_ID,
        vec![Uuid::from_u128(0xA)],
    )]));
    let composed = compose_query_with_scope(&user, &scope).expect("compose ok");

    // The filter AST genuinely changed (the tenant predicate was AND-merged):
    // its hash differs from the user-filter hash.
    assert_ne!(
        toolkit_odata::short_filter_hash(composed.filter()),
        user_hash,
        "scope narrowing must change the filter AST",
    );
    // But the stored filter_hash stays pinned to the USER-filter hash, so the
    // keyset cursor's `f` keeps matching the gateway's hash across pages.
    assert_eq!(
        composed.filter_hash, user_hash,
        "compose MUST NOT re-hash the server-injected scope into filter_hash",
    );
}

#[test]
fn compose_unconstrained_scope_is_denied_fail_closed() {
    // An `allow_all` scope on the LIST/aggregate path is a degenerate
    // empty-predicate permit, not a happy-path admin grant. Composition MUST
    // fail closed (via `scope_to_odata_filter`) rather than pass the user
    // filter through unscoped, which would return every tenant's records.
    let user = user_query_with_gateway_hash(
        "created_at ge 2026-01-01T00:00:00Z and created_at lt 2026-02-01T00:00:00Z",
    );
    let err = compose_query_with_scope(&user, &AccessScope::allow_all())
        .expect_err("allow_all -> PermissionDenied");
    assert!(
        matches!(err, UsageCollectorError::PermissionDenied { .. }),
        "allow_all must surface as PermissionDenied, got {err:?}",
    );
}
