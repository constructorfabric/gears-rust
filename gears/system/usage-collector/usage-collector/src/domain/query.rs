//! Query read-path helpers for the usage-collector domain service.
//!
//! Holds the helpers that serve only the `list_usage_records` /
//! `query_aggregated_usage_records` read paths, kept out of `service.rs`
//! so it stays focused on orchestration:
//!
//! * `compose_query_with_scope` — AND-merges the PDP-returned
//!   [`AccessScope`] into the caller's `$filter`.
//! * `require_bounded_time_window` — rejects an unbounded `created_at`
//!   window before composition / dispatch.

use toolkit_odata::{ODataQuery, ast};
use toolkit_security::AccessScope;
use usage_collector_sdk::UsageCollectorError;

use crate::domain::authz;

/// AND-merge the PDP-returned [`AccessScope`] into the caller's
/// [`ODataQuery`] filter under intersection-only semantics, returning a
/// fresh query ready for plugin dispatch.
///
/// `composed_filter = user_filter AND scope_filter`. The scope always
/// contributes a narrowing predicate: [`authz::scope_to_odata_filter`]
/// fails closed on an unconstrained / empty-constraint / deny-all scope
/// rather than yielding a pass-through, so there is no "filter unchanged"
/// branch. When the user supplied no filter the scope filter alone becomes
/// the composed filter. `gts_id`, the time window, and the order /
/// limit / cursor / select projections on [`ODataQuery`] flow through
/// verbatim — the composition only touches the `$filter` AST.
///
/// Per `cpt-cf-usage-collector-algo-usage-query-pdp-constraint-composition-v2`:
/// composition is intersection-only (no widening). PDP constraint
/// shapes outside the supported set (tree predicates, unknown
/// properties, value-type mismatches) bubble up as fail-closed
/// [`AuthorizationDenied`](crate::domain::DomainError::AuthorizationDenied) from
/// [`authz::scope_to_odata_filter`].
// @cpt-algo:cpt-cf-usage-collector-algo-usage-query-pdp-constraint-composition-v2:p2
pub(crate) fn compose_query_with_scope(
    user_query: &ODataQuery,
    scope: &AccessScope,
) -> Result<ODataQuery, UsageCollectorError> {
    // @cpt-begin:cpt-cf-usage-collector-algo-usage-query-pdp-constraint-composition-v2:p2:inst-constraint-composition-parse-pdp
    // @cpt-begin:cpt-cf-usage-collector-algo-usage-query-pdp-constraint-composition-v2:p2:inst-constraint-composition-iterate
    // `scope_to_odata_filter` always yields a narrowing predicate or fails
    // closed: an unconstrained / empty-constraint / deny-all scope is denied,
    // never passed through as "no row narrowing".
    let scope_expr = authz::scope_to_odata_filter(scope).map_err(UsageCollectorError::from)?;
    // @cpt-end:cpt-cf-usage-collector-algo-usage-query-pdp-constraint-composition-v2:p2:inst-constraint-composition-iterate
    // @cpt-end:cpt-cf-usage-collector-algo-usage-query-pdp-constraint-composition-v2:p2:inst-constraint-composition-parse-pdp

    // @cpt-begin:cpt-cf-usage-collector-algo-usage-query-pdp-constraint-composition-v2:p2:inst-constraint-composition-intersect
    let composed_filter: ast::Expr = match user_query.filter().cloned() {
        Some(user_expr) => user_expr.and(scope_expr),
        None => scope_expr,
    };
    // @cpt-end:cpt-cf-usage-collector-algo-usage-query-pdp-constraint-composition-v2:p2:inst-constraint-composition-intersect

    let mut composed = user_query.clone();
    // Preserve the caller's `filter_hash` (the hash of the *user* `$filter`,
    // computed by the gateway) — do NOT re-hash the AND-merged filter. The
    // keyset cursor's `f` field exists to detect the *user* changing their
    // `$filter` between paginated requests; the PDP scope AND-merged here is
    // server-injected and not user-controlled, so it MUST be excluded from the
    // hash. Both cursor validators (the gateway, pre-composition, and the
    // plugin's `cursor.f == query.filter_hash` check) compare against the
    // user-filter hash, and the plugin embeds `query.filter_hash` into the
    // next_cursor. Re-hashing to `hash(user AND scope)` here would embed a hash
    // the gateway's `hash(user)` can never match on the follow-up request —
    // breaking keyset pagination with a spurious `FILTER_MISMATCH` 400 the
    // moment PDP returns any row scope (latent until LIST began requiring
    // constraints). `composed` keeps `user_query.filter_hash` from the clone.
    composed.filter = Some(Box::new(composed_filter));
    // @cpt-begin:cpt-cf-usage-collector-algo-usage-query-pdp-constraint-composition-v2:p2:inst-constraint-composition-return
    Ok(composed)
    // @cpt-end:cpt-cf-usage-collector-algo-usage-query-pdp-constraint-composition-v2:p2:inst-constraint-composition-return
}

/// The `UsageRecord` field carrying the query time window. The bounded
/// `[from, to)` window is expressed in the `$filter` AST as
/// `created_at ge … and created_at lt …`.
const CREATED_AT_FIELD: &str = "created_at";

/// Require that `query`'s `$filter` pins a bounded `created_at` window:
/// at least one lower bound (`created_at ge|gt …`) **and** at least one
/// upper bound (`created_at le|lt …`) must appear as top-level
/// conjuncts. Without both, the query would drive an unbounded
/// full-table scan / aggregation — a `DoS` / cost footgun — so it is
/// rejected with [`UsageCollectorError::missing_time_window`].
///
/// Only **top-level conjuncts** count: a bound nested under an `or` /
/// `not` does not constrain the scan (rows outside the window can still
/// match), so the AND-chain is flattened and only its leaves are
/// inspected. The bound's value type is not checked here — a malformed
/// literal is a type mismatch caught downstream by the plugin's filter
/// conversion, not a missing-window error.
///
/// Enforced on the shared service path (not just the REST handler) so
/// in-process SDK callers and out-of-process REST callers obtain the
/// same guarantee, per the single-authorization-path contract.
pub(crate) fn require_bounded_time_window(query: &ODataQuery) -> Result<(), UsageCollectorError> {
    let Some(filter) = query.filter() else {
        return Err(UsageCollectorError::missing_time_window());
    };

    let mut has_lower = false;
    let mut has_upper = false;
    visit_top_level_conjuncts(filter, &mut |conjunct| {
        if let ast::Expr::Compare(left, op, _right) = conjunct
            && let ast::Expr::Identifier(name) = left.as_ref()
            && name.eq_ignore_ascii_case(CREATED_AT_FIELD)
        {
            match op {
                ast::CompareOperator::Ge | ast::CompareOperator::Gt => has_lower = true,
                ast::CompareOperator::Le | ast::CompareOperator::Lt => has_upper = true,
                _ => {}
            }
        }
    });

    if has_lower && has_upper {
        Ok(())
    } else {
        Err(UsageCollectorError::missing_time_window())
    }
}

/// Walk the top-level conjunction of `expr`, invoking `visit` on each
/// conjunct. `And` nodes are flattened transparently; any other node
/// (a leaf comparison, or an `or` / `not` / function subtree) is passed
/// to `visit` as a single opaque conjunct.
fn visit_top_level_conjuncts(expr: &ast::Expr, visit: &mut impl FnMut(&ast::Expr)) {
    match expr {
        ast::Expr::And(left, right) => {
            visit_top_level_conjuncts(left, visit);
            visit_top_level_conjuncts(right, visit);
        }
        other => visit(other),
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "query_tests.rs"]
mod query_tests;
