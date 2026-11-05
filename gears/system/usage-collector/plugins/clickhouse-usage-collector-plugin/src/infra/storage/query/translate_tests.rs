use toolkit_odata::filter::{FilterField, FilterNode, FilterOp, ODataValue};
use usage_collector_sdk::UsageRecordFilterField;

use super::{
    SqlBind, SqlCtx, and_conjuncts, record_column, translate_record_filter,
    translate_usage_type_filter, usage_type_column,
};

fn record_node_status_eq_active() -> FilterNode<UsageRecordFilterField> {
    let field = <UsageRecordFilterField as FilterField>::from_name("status").unwrap();
    FilterNode::Binary {
        field,
        op: FilterOp::Eq,
        value: ODataValue::String("active".to_owned()),
    }
}

/// A `status <op> 'active'` binary node, for exercising operator mapping.
fn record_node_status_op(op: FilterOp) -> FilterNode<UsageRecordFilterField> {
    let field = <UsageRecordFilterField as FilterField>::from_name("status").unwrap();
    FilterNode::Binary {
        field,
        op,
        value: ODataValue::String("active".to_owned()),
    }
}

#[test]
fn status_eq_active_yields_parameterised_fragment() {
    let mut ctx = SqlCtx::new();
    let frag = translate_record_filter(&record_node_status_eq_active(), &mut ctx).unwrap();
    assert_eq!(frag, "status = ?");
    assert_eq!(ctx.binds.len(), 1);
    assert!(matches!(&ctx.binds[0], SqlBind::Str(s) if s == "active"));
}

#[test]
fn in_list_yields_correct_placeholders() {
    let field = <UsageRecordFilterField as FilterField>::from_name("status").unwrap();
    let node: FilterNode<UsageRecordFilterField> = FilterNode::InList {
        field,
        values: vec![
            ODataValue::String("active".to_owned()),
            ODataValue::String("inactive".to_owned()),
        ],
    };

    let mut ctx = SqlCtx::new();
    let frag = translate_record_filter(&node, &mut ctx).unwrap();
    assert_eq!(frag, "status IN (?, ?)");
    assert_eq!(ctx.binds.len(), 2);
}

/// A one-element `IN` is an equality. The gateway's authorization scope sends
/// `tenant_id in (<one tenant>)` on every request, and only an equality lets
/// `ClickHouse` treat the sorting-key prefix as fixed — which is what makes the
/// list read in key order and stop at its `LIMIT` (measured ~11x fewer rows).
#[test]
fn single_element_in_list_is_emitted_as_an_equality() {
    let field = <UsageRecordFilterField as FilterField>::from_name("tenant_id").unwrap();
    let tenant = uuid::Uuid::from_u128(0xA0);
    let node: FilterNode<UsageRecordFilterField> = FilterNode::InList {
        field,
        values: vec![ODataValue::Uuid(tenant)],
    };

    let mut ctx = SqlCtx::new();
    let frag = translate_record_filter(&node, &mut ctx).unwrap();
    assert_eq!(
        frag, "tenant_id = ?",
        "one value must not render as `IN (?)`"
    );
    assert_eq!(ctx.binds.len(), 1);
    assert!(matches!(ctx.binds[0], SqlBind::Uuid(u) if u == tenant));
}

/// The equality rewrite keeps the bind's own placeholder shape, so a
/// `DateTime64` value still goes through `fromUnixTimestamp64Micro(?)`.
#[test]
fn single_element_in_list_keeps_the_typed_placeholder() {
    let field = <UsageRecordFilterField as FilterField>::from_name("created_at").unwrap();
    let node: FilterNode<UsageRecordFilterField> = FilterNode::InList {
        field,
        values: vec![ODataValue::DateTime(
            chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
        )],
    };

    let mut ctx = SqlCtx::new();
    let frag = translate_record_filter(&node, &mut ctx).unwrap();
    assert_eq!(frag, "created_at = fromUnixTimestamp64Micro(?)");
    assert_eq!(ctx.binds.len(), 1);
}

#[test]
fn unknown_field_is_rejected() {
    // Build a binary node for a field not in the allowlist.
    // `UsageTypeFilterField` has `gts_id` and `kind`; use `gts_id` against the
    // record translate fn (which only allows record columns).
    use usage_collector_sdk::UsageTypeFilterField;
    let field = <UsageTypeFilterField as FilterField>::from_name("gts_id").unwrap();
    let node = FilterNode::Binary {
        field,
        op: FilterOp::Eq,
        value: ODataValue::String("some-gts-id".to_owned()),
    };
    let mut ctx = SqlCtx::new();
    // usage_type_column accepts "gts_id", but record_column does not.
    let result = super::translate_filter(&node, &mut ctx, record_column);
    assert!(result.is_err());
}

#[test]
fn usage_type_column_accepts_gts_id_and_kind() {
    assert_eq!(usage_type_column("gts_id"), Some("gts_id"));
    assert_eq!(usage_type_column("kind"), Some("kind"));
    assert_eq!(usage_type_column("unknown"), None);
}

#[test]
fn composite_and_yields_parenthesised_and() {
    let a = record_node_status_eq_active();
    let b = record_node_status_eq_active();
    let node = FilterNode::Composite {
        op: FilterOp::And,
        children: vec![a, b],
    };
    let mut ctx = SqlCtx::new();
    let frag = translate_record_filter(&node, &mut ctx).unwrap();
    assert_eq!(frag, "(status = ? AND status = ?)");
    assert_eq!(ctx.binds.len(), 2);
}

// ── Operator mapping ─────────────────────────────────────────────────────────

/// Every comparison operator maps to its SQL spelling, and the value is always
/// a `?` bind rather than interpolated text.
#[test]
fn every_comparison_operator_maps_to_its_sql_spelling() {
    for (op, sql) in [
        (FilterOp::Eq, "="),
        (FilterOp::Ne, "<>"),
        (FilterOp::Gt, ">"),
        (FilterOp::Ge, ">="),
        (FilterOp::Lt, "<"),
        (FilterOp::Le, "<="),
    ] {
        let mut ctx = SqlCtx::new();
        let frag = translate_record_filter(&record_node_status_op(op), &mut ctx).unwrap();
        assert_eq!(frag, format!("status {sql} ?"), "op = {op:?}");
        assert_eq!(ctx.binds.len(), 1, "op = {op:?} binds its value");
    }
}

/// String-matching and set operators are handled structurally (`InList`) or not
/// at all — reaching `op_sql` with one is a translation error, never a
/// silently-dropped predicate.
#[test]
fn non_comparison_operators_are_rejected_by_binary_translation() {
    for op in [
        FilterOp::In,
        FilterOp::Contains,
        FilterOp::StartsWith,
        FilterOp::EndsWith,
        FilterOp::And,
        FilterOp::Or,
    ] {
        let mut ctx = SqlCtx::new();
        let err = translate_record_filter(&record_node_status_op(op), &mut ctx)
            .expect_err("non-comparison operator must not translate as a binary comparison");
        assert!(
            err.contains("unsupported operator"),
            "op = {op:?} must report an unsupported operator, got: {err}"
        );
    }
}

// ── usage_type translation ───────────────────────────────────────────────────

/// The catalog translator resolves identifiers through `usage_type_column`, so a
/// catalog field translates here even though it is not a record column.
#[test]
fn usage_type_filter_translates_catalog_fields() {
    use usage_collector_sdk::UsageTypeFilterField;

    let field = <UsageTypeFilterField as FilterField>::from_name("kind").unwrap();
    let node = FilterNode::Binary {
        field,
        op: FilterOp::Eq,
        value: ODataValue::String("counter".to_owned()),
    };

    let mut ctx = SqlCtx::new();
    let frag = translate_usage_type_filter(&node, &mut ctx).unwrap();
    assert_eq!(frag, "kind = ?");
    assert!(matches!(&ctx.binds[0], SqlBind::Str(s) if s == "counter"));
}

/// A record-only field is not on the catalog allowlist, so it can never reach
/// the catalog SQL string.
#[test]
fn usage_type_filter_rejects_record_only_field() {
    let field = <UsageRecordFilterField as FilterField>::from_name("tenant_id").unwrap();
    let node = FilterNode::Binary {
        field,
        op: FilterOp::Eq,
        value: ODataValue::String("whatever".to_owned()),
    };

    let mut ctx = SqlCtx::new();
    let err = translate_usage_type_filter(&node, &mut ctx)
        .expect_err("tenant_id is not a catalog column");
    assert!(
        err.contains("field not allowlisted"),
        "expected an allowlist rejection, got: {err}"
    );
}

// ── Structural edge cases ────────────────────────────────────────────────────

/// An empty `IN ()` is not valid SQL, so it is rejected rather than emitted.
#[test]
fn empty_in_list_is_rejected() {
    let field = <UsageRecordFilterField as FilterField>::from_name("status").unwrap();
    let node: FilterNode<UsageRecordFilterField> = FilterNode::InList {
        field,
        values: Vec::new(),
    };

    let mut ctx = SqlCtx::new();
    let err = translate_record_filter(&node, &mut ctx).expect_err("empty IN list must be rejected");
    assert!(
        err.contains("IN list must not be empty"),
        "unexpected error: {err}"
    );
    assert!(ctx.binds.is_empty(), "a rejected node pushes no binds");
}

#[test]
fn composite_or_joins_children_with_or() {
    let node = FilterNode::Composite {
        op: FilterOp::Or,
        children: vec![
            record_node_status_eq_active(),
            record_node_status_op(FilterOp::Ne),
        ],
    };
    let mut ctx = SqlCtx::new();
    let frag = translate_record_filter(&node, &mut ctx).unwrap();
    assert_eq!(frag, "(status = ? OR status <> ?)");
    assert_eq!(ctx.binds.len(), 2);
}

/// A composite carrying a comparison operator is malformed input; only
/// `And`/`Or` can join children.
#[test]
fn composite_with_comparison_operator_is_rejected() {
    let node = FilterNode::Composite {
        op: FilterOp::Eq,
        children: vec![record_node_status_eq_active()],
    };
    let mut ctx = SqlCtx::new();
    let err = translate_record_filter(&node, &mut ctx)
        .expect_err("a comparison operator cannot join composite children");
    assert!(
        err.contains("invalid composite operator"),
        "unexpected error: {err}"
    );
}

#[test]
fn not_wraps_inner_fragment() {
    let node = FilterNode::Not(Box::new(record_node_status_eq_active()));
    let mut ctx = SqlCtx::new();
    let frag = translate_record_filter(&node, &mut ctx).unwrap();
    assert_eq!(frag, "NOT (status = ?)");
    assert_eq!(
        ctx.binds.len(),
        1,
        "the negated child still binds its value"
    );
}

#[test]
fn default_ctx_starts_empty() {
    let ctx = SqlCtx::default();
    assert!(ctx.binds.is_empty());
}

// ── and_conjuncts ────────────────────────────────────────────────────────────

/// A record filter field by name.
fn field(name: &str) -> UsageRecordFilterField {
    UsageRecordFilterField::from_name(name).expect("field must be on the record filter schema")
}

/// `<name> eq 'x'` — the value is irrelevant to the flattening.
fn eq(name: &str) -> FilterNode<UsageRecordFilterField> {
    FilterNode::binary(
        field(name),
        FilterOp::Eq,
        ODataValue::String("x".to_owned()),
    )
}

/// The field each conjunct names, or a marker for a subtree kept whole.
fn names(nodes: &[&FilterNode<UsageRecordFilterField>]) -> Vec<&'static str> {
    nodes
        .iter()
        .map(|node| match node {
            FilterNode::Binary { field, .. } | FilterNode::InList { field, .. } => field.name(),
            FilterNode::Composite { .. } => "<composite>",
            FilterNode::Not(_) => "<not>",
        })
        .collect()
}

/// The shape the gateway actually sends: the authorization scope, the
/// `[from, to)` window and a status filter, all as top-level conjuncts. Each
/// becomes its own `WHERE` clause in the caller's order.
#[test]
fn gateway_filter_is_flattened_into_its_conjuncts_in_order() {
    let node = FilterNode::and(vec![
        eq("tenant_id"),
        FilterNode::binary(
            field("created_at"),
            FilterOp::Ge,
            ODataValue::String("2026-01-01T00:00:00Z".to_owned()),
        ),
        FilterNode::binary(
            field("created_at"),
            FilterOp::Lt,
            ODataValue::String("2026-02-01T00:00:00Z".to_owned()),
        ),
        eq("status"),
    ]);
    assert_eq!(
        names(&and_conjuncts(&node)),
        vec!["tenant_id", "created_at", "created_at", "status"]
    );
}

#[test]
fn nested_and_spine_is_flattened() {
    let node = FilterNode::and(vec![
        eq("tenant_id"),
        FilterNode::and(vec![eq("resource_id"), FilterNode::and(vec![eq("status")])]),
    ]);
    assert_eq!(
        names(&and_conjuncts(&node)),
        vec!["tenant_id", "resource_id", "status"]
    );
}

/// `OR` and `NOT` are not conjunctions: they stay whole, so `AND`-joining the
/// conjuncts is exactly the caller's filter.
#[test]
fn or_and_not_subtrees_stay_whole() {
    let node = FilterNode::and(vec![
        FilterNode::or(vec![eq("status"), eq("resource_id")]),
        FilterNode::not(eq("tenant_id")),
    ]);
    assert_eq!(names(&and_conjuncts(&node)), vec!["<composite>", "<not>"]);

    let bare_or = FilterNode::or(vec![eq("tenant_id"), eq("resource_id")]);
    assert_eq!(names(&and_conjuncts(&bare_or)), vec!["<composite>"]);
}

/// An empty `AND` spine yields no conjuncts at all, so a caller emits no clause
/// for it. Translating it whole would render a bare `()`, which `ClickHouse`
/// rejects as a syntax error.
#[test]
fn empty_and_yields_no_conjuncts() {
    let node: FilterNode<UsageRecordFilterField> = FilterNode::and(Vec::new());
    assert!(and_conjuncts(&node).is_empty());
}
