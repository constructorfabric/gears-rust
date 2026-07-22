//! Aggregation SQL (Phase 4): inject-safe SELECT-expression builders for the
//! pushed-down `aggregate` query.
//!
//! `aggregate` assembles `SELECT <dim exprs…>, <AGG> FROM usage_records WHERE
//! gts_id = $1 AND status = 'active' [AND …] [GROUP BY 1, 2, …]`. The two
//! helpers here own the two kinds of SELECT expression:
//!
//! - [`agg_select_expr`] — the aggregate column. Every variant casts to
//!   `numeric` (`COUNT(*)::numeric`, `SUM(value)::numeric`,
//!   `MIN/MAX(value)::numeric`, `ROUND(AVG(value), 6)::numeric`) so the result
//!   reads back uniformly as `Option<BigDecimal>` regardless of the chosen op.
//!   Reading into arbitrary-precision `bigdecimal::BigDecimal` (the SDK's
//!   `AggregationBucket.value` type) removes the former `rust_decimal::Decimal`
//!   ~7.9×10²⁸ ceiling that turned a wide `SUM` (or large-magnitude `AVG`) into
//!   an `Internal`/500 on decode. `AVG` is still `ROUND`-ed to 6 fractional
//!   digits — a plugin-chosen rounding scale the SDK explicitly sanctions —
//!   because a non-terminating quotient (e.g. `÷ 3`) is unbounded in scale;
//!   arbitrary precision is still finite, so a scale bound is needed regardless.
//! - [`dimension_select_expr`] — a group dimension as a TEXT-returning expr.
//!
//! All identifiers come from the closed [`AggregationOp`] /
//! [`AggregationDimension`] enum matches (an allowlist — never caller text), so
//! no identifier is interpolated from untrusted input. The only caller-derived
//! value, a [`AggregationDimension::Metadata`] key, is bound (`$N`) via the
//! shared [`SqlCtx`].

use usage_collector_sdk::{AggregationDimension, AggregationOp, MAX_AGGREGATION_BUCKETS};

use super::bind::SqlBind;
use super::translate::SqlCtx;

/// SQL aggregate expression for an [`AggregationOp`].
///
/// Every op casts to `numeric` so the result — including the integer-typed
/// `COUNT(*)` — reads back uniformly as `Option<BigDecimal>` in `aggregate`.
/// The returned string is a `'static` constant from the closed enum match,
/// never caller text.
#[must_use]
pub fn agg_select_expr(op: AggregationOp) -> &'static str {
    match op {
        AggregationOp::Sum => "SUM(value)::numeric",
        AggregationOp::Count => "COUNT(*)::numeric",
        AggregationOp::Min => "MIN(value)::numeric",
        AggregationOp::Max => "MAX(value)::numeric",
        // `ROUND(.., 6)` caps the fractional scale so a non-terminating
        // quotient (e.g. `÷ 3`) stays finite; arbitrary-precision `BigDecimal`
        // removes the old magnitude ceiling but not the need to bound scale
        // (see module doc).
        AggregationOp::Avg => "ROUND(AVG(value), 6)::numeric",
    }
}

/// `corrects_id`-partition WHERE clause for an [`AggregationOp`], or `None`.
///
/// Per the plugin-spi.md §Method 3 aggregation contract, across the accepted
/// active-row scope:
///
/// - `SUM` MUST net across rows regardless of `corrects_id` — compensation
///   entries carry a signed `value` and reduce the running total — so it gets
///   **no** partition (`None`).
/// - Every other op (`COUNT`, `MIN`, `MAX`, `AVG`) MUST operate over
///   `corrects_id IS NULL` rows only: compensations adjust `SUM`, they are not
///   events, so including them would double-count (`COUNT`) or corrupt
///   extremes/means (`MIN`/`MAX`/`AVG`).
///
/// Applied uniformly as the spec-sanctioned defence-in-depth form ("`SUM` nets;
/// every other op filters `corrects_id IS NULL`"). Under the op-per-kind
/// restriction the partition is load-bearing only for `COUNT`-on-counter —
/// counters are where active compensation rows accumulate; `MIN`/`MAX`/`AVG`
/// are gauge-only and gauges never carry compensations, so the clause is a
/// structural no-op there. The returned string is a `'static` constant, never
/// caller text.
#[must_use]
pub fn corrects_id_partition_clause(op: AggregationOp) -> Option<&'static str> {
    match op {
        AggregationOp::Sum => None,
        AggregationOp::Count | AggregationOp::Min | AggregationOp::Max | AggregationOp::Avg => {
            Some("corrects_id IS NULL")
        }
    }
}

/// SQL TEXT-returning expression for a group [`AggregationDimension`].
///
/// The identity columns map through the closed enum match (an allowlist), so
/// the only caller-derived value — the [`AggregationDimension::Metadata`] key —
/// is bound via `ctx` (`metadata ->> $N`) rather than interpolated. `tenant_id`
/// is a `uuid` column, so it is cast to `text` for a uniform `Option<String>`
/// positional read in `aggregate`.
///
/// Returns the SELECT expression string (used positionally; the `GROUP BY`
/// references it by ordinal so the bound metadata expr is never repeated).
pub fn dimension_select_expr(dim: &AggregationDimension, ctx: &mut SqlCtx) -> String {
    match dim {
        AggregationDimension::TenantId => "tenant_id::text".to_owned(),
        AggregationDimension::ResourceId => "resource_id".to_owned(),
        AggregationDimension::ResourceType => "resource_type".to_owned(),
        AggregationDimension::SubjectId => "subject_id".to_owned(),
        AggregationDimension::SubjectType => "subject_type".to_owned(),
        AggregationDimension::Metadata(key) => {
            let n = ctx.push(SqlBind::Str(key.as_str().to_owned()));
            format!("metadata ->> ${n}")
        }
    }
}

/// `LIMIT` clause bounding the aggregate's distinct-group cardinality.
///
/// The gateway-enforced bounded window caps the rows scanned; it does not cap
/// the distinct groups a `GROUP BY` produces. A high-cardinality
/// [`AggregationDimension::Metadata`] key (e.g. a per-record id) could otherwise
/// materialize an unbounded bucket set into memory, unlike the page-size-clamped
/// list path. `LIMIT MAX_AGGREGATION_BUCKETS + 1` bounds that; the `+ 1` lets the
/// gateway tell "exactly at the cap" from "over the cap" and reject the latter
/// with a `400` ([`usage_collector_sdk::reason::AGGREGATION_RESULT_TOO_LARGE`]).
/// The no-grouping case (`dim_count == 0`) is a single aggregate row and needs no
/// cap, so it yields an empty clause.
#[must_use]
pub fn aggregate_limit_clause(dim_count: usize) -> String {
    if dim_count == 0 {
        String::new()
    } else {
        format!(" LIMIT {}", MAX_AGGREGATION_BUCKETS + 1)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "aggregate_tests.rs"]
mod aggregate_tests;
