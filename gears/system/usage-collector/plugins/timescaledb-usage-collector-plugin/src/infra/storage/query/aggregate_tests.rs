//! Unit tests for the aggregation SELECT-expression builders. Pure (no DB):
//! they pin the exact SQL each [`AggregationOp`] emits, so a cast regression is
//! caught without Docker.

use usage_collector_sdk::{
    AggregationDimension, AggregationOp, MAX_AGGREGATION_BUCKETS, MetadataKey,
};

use super::super::bind::SqlBind;
use super::super::translate::SqlCtx;
use super::{
    agg_select_expr, aggregate_limit_clause, corrects_id_partition_clause, dimension_select_expr,
};

#[test]
fn every_aggregate_op_casts_to_numeric() {
    assert_eq!(agg_select_expr(AggregationOp::Sum), "SUM(value)::numeric");
    assert_eq!(agg_select_expr(AggregationOp::Count), "COUNT(*)::numeric");
    assert_eq!(agg_select_expr(AggregationOp::Min), "MIN(value)::numeric");
    assert_eq!(agg_select_expr(AggregationOp::Max), "MAX(value)::numeric");
    // `AVG` is rounded to bound the fractional scale within `rust_decimal`'s
    // ~28-digit capacity (a non-terminating quotient otherwise fails to decode).
    assert_eq!(
        agg_select_expr(AggregationOp::Avg),
        "ROUND(AVG(value), 6)::numeric"
    );
}

#[test]
fn limit_clause_caps_grouped_queries_at_cap_plus_one() {
    // The `+ 1` is load-bearing: it lets the gateway distinguish a result
    // exactly at the cap (allowed) from one over it (rejected 400).
    let expected = format!(" LIMIT {}", MAX_AGGREGATION_BUCKETS + 1);
    assert_eq!(aggregate_limit_clause(1), expected);
    assert_eq!(aggregate_limit_clause(2), expected);
}

#[test]
fn limit_clause_absent_for_no_grouping() {
    // No `group_by` → a single aggregate row; no cardinality to bound.
    assert!(aggregate_limit_clause(0).is_empty());
}

#[test]
fn corrects_id_partition_applies_to_every_op_but_sum() {
    // plugin-spi.md §Method 3 aggregation contract: `SUM` nets across all
    // active rows (compensations carry a signed `value`), so it gets no
    // `corrects_id` partition; every other op MUST operate over
    // `corrects_id IS NULL` rows only (compensations adjust `SUM`, they are
    // not events). Load-bearing for `COUNT`-on-counter; a structural no-op
    // for the gauge-only MIN/MAX/AVG, applied uniformly as sanctioned
    // defence-in-depth.
    assert_eq!(corrects_id_partition_clause(AggregationOp::Sum), None);
    for op in [
        AggregationOp::Count,
        AggregationOp::Min,
        AggregationOp::Max,
        AggregationOp::Avg,
    ] {
        assert_eq!(
            corrects_id_partition_clause(op),
            Some("corrects_id IS NULL"),
            "{op:?} must exclude compensation rows"
        );
    }
}

// ── dimension select-expr ────────────────────────────────────────────────────

#[test]
fn metadata_dimension_binds_the_key_and_emits_json_extract() {
    // The one aggregation builder that touches caller-derived input: the
    // metadata dimension key MUST be bound (`metadata ->> $N`), never
    // interpolated. A regression that inlined the key would surface here as a
    // changed expr string and/or a missing bind.
    let dim = AggregationDimension::Metadata(MetadataKey::new("region").unwrap());
    let mut ctx = SqlCtx::new(1);
    let expr = dimension_select_expr(&dim, &mut ctx);
    assert_eq!(expr, "metadata ->> $1");
    assert_eq!(ctx.binds.len(), 1, "exactly one bind is pushed");
    assert!(
        matches!(&ctx.binds[0], SqlBind::Str(s) if s == "region"),
        "the caller-derived key is bound verbatim as text, got {:?}",
        ctx.binds[0]
    );
}

#[test]
fn metadata_dimension_placeholder_honors_start_offset() {
    // The placeholder index comes from `ctx`, not a hardcoded `$1`, so the
    // dimension expr composes correctly after leading binds (e.g. `gts_id` at
    // `$1`).
    let dim = AggregationDimension::Metadata(MetadataKey::new("tier").unwrap());
    let mut ctx = SqlCtx::new(3);
    assert_eq!(dimension_select_expr(&dim, &mut ctx), "metadata ->> $3");
}

#[test]
fn identity_dimensions_emit_static_columns_and_bind_nothing() {
    // Every non-metadata dimension is a closed-enum `'static` column (an
    // allowlist), so none may push a bind.
    for (dim, expected) in [
        (AggregationDimension::TenantId, "tenant_id::text"),
        (AggregationDimension::ResourceId, "resource_id"),
        (AggregationDimension::ResourceType, "resource_type"),
        (AggregationDimension::SubjectId, "subject_id"),
        (AggregationDimension::SubjectType, "subject_type"),
    ] {
        let mut ctx = SqlCtx::new(1);
        assert_eq!(dimension_select_expr(&dim, &mut ctx), expected, "{dim:?}");
        assert!(ctx.binds.is_empty(), "{dim:?} must bind nothing");
    }
}
