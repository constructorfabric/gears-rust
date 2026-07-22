// Test modules using bare `panic!` opt in explicitly
// (clippy.toml allows unwrap/expect in tests, not panic).
#![allow(clippy::panic)]

use std::str::FromStr;

use bigdecimal::BigDecimal;
use chrono::TimeZone;

use toolkit_odata::filter::{FieldKind, FilterField, FilterNode, FilterOp};
use toolkit_odata::{CursorV1, ODataOrderBy, OrderKey, SortDir};
use usage_collector_sdk::UsageRecordFilterField;

use super::super::bind::{SqlBind, odata_value_to_bind};
use super::super::keyset::{
    cursor_key_to_bind, ensure_forward_cursor, keyset_predicate, render_order_by,
};
use super::{ODataValue, SqlCtx, record_column, translate_record_filter, usage_type_column};

// ── Helpers ────────────────────────────────────────────────────────────────

fn rec_field(name: &str) -> UsageRecordFilterField {
    <UsageRecordFilterField as FilterField>::from_name(name)
        .unwrap_or_else(|| panic!("unknown record field `{name}`"))
}

/// Resolve a record field name to its declared [`FieldKind`] — the keyset bind
/// resolver the record store passes to [`keyset_predicate`].
fn rec_kind(name: &str) -> Option<FieldKind> {
    <UsageRecordFilterField as FilterField>::from_name(name).map(|f| f.kind())
}

/// Fail-closed "is this record field a never-null (keyset-safe) column"
/// predicate the record store passes to [`keyset_predicate`].
fn rec_keyset_safe(name: &str) -> bool {
    usage_collector_sdk::is_keyset_safe_record_field(name)
}

fn binary(name: &str, op: FilterOp, value: ODataValue) -> FilterNode<UsageRecordFilterField> {
    FilterNode::Binary {
        field: rec_field(name),
        op,
        value,
    }
}

fn uuid_val() -> ODataValue {
    ODataValue::Uuid(uuid::Uuid::from_u128(0x1234))
}

fn dt_val() -> ODataValue {
    ODataValue::DateTime(chrono::Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap())
}

// ── Column allowlist ─────────────────────────────────────────────────────────

#[test]
fn record_field_columns_are_allowlisted() {
    // The record identity column was renamed `uuid` -> `id` (migration 0002),
    // so `id` maps to itself and bare `uuid` is not an allowlisted field name.
    assert_eq!(record_column("id"), Some("id"));
    assert_eq!(record_column("uuid"), None);
    assert_eq!(record_column("created_at"), Some("created_at"));
    assert_eq!(record_column("tenant_id"), Some("tenant_id"));
    assert_eq!(record_column("resource_id"), Some("resource_id"));
    assert_eq!(record_column("resource_type"), Some("resource_type"));
    assert_eq!(record_column("subject_id"), Some("subject_id"));
    assert_eq!(record_column("subject_type"), Some("subject_type"));
    assert_eq!(record_column("corrects_id"), Some("corrects_id"));
    assert_eq!(record_column("status"), Some("status"));
    assert_eq!(record_column("definitely_not_a_column"), None);
}

#[test]
fn usage_type_columns_are_allowlisted() {
    assert_eq!(usage_type_column("gts_id"), Some("gts_id"));
    assert_eq!(usage_type_column("kind"), Some("kind"));
    assert_eq!(usage_type_column("gts_id; DROP TABLE"), None);
}

// ── Value conversion ─────────────────────────────────────────────────────────

#[test]
fn number_converts_to_decimal_bind() {
    let v =
        odata_value_to_bind(&ODataValue::Number(BigDecimal::from_str("42.5").unwrap())).unwrap();
    assert!(matches!(v, SqlBind::Decimal(d) if d.to_string() == "42.5"));
}

#[test]
fn datetime_converts_to_offsetdatetime_bind() {
    let v = odata_value_to_bind(&dt_val()).unwrap();
    assert!(matches!(v, SqlBind::DateTime(_)));
}

#[test]
fn null_and_date_and_time_values_are_rejected() {
    assert!(odata_value_to_bind(&ODataValue::Null).is_err());
    assert!(
        odata_value_to_bind(&ODataValue::Date(
            chrono::NaiveDate::from_ymd_opt(2026, 1, 2).unwrap()
        ))
        .is_err()
    );
    assert!(
        odata_value_to_bind(&ODataValue::Time(
            chrono::NaiveTime::from_hms_opt(1, 2, 3).unwrap()
        ))
        .is_err()
    );
}

#[test]
fn bool_and_uuid_values_convert_to_their_binds() {
    assert!(matches!(
        odata_value_to_bind(&ODataValue::Bool(true)).unwrap(),
        SqlBind::Bool(true)
    ));
    let u = uuid::Uuid::from_u128(0x1234);
    assert!(matches!(
        odata_value_to_bind(&ODataValue::Uuid(u)).unwrap(),
        SqlBind::Uuid(got) if got == u
    ));
}

#[test]
fn numeric_out_of_decimal_range_is_rejected() {
    // 40-digit integer: well past rust_decimal::Decimal's 96-bit mantissa, so
    // the `BigDecimal` -> `Decimal` conversion must surface an error rather than
    // silently truncate.
    let huge = BigDecimal::from_str("1000000000000000000000000000000000000000").unwrap();
    assert!(odata_value_to_bind(&ODataValue::Number(huge)).is_err());
}

// ── Filter translation ───────────────────────────────────────────────────────

#[test]
fn binary_eq_renders_single_placeholder_and_one_bind() {
    let node = binary(
        "status",
        FilterOp::Eq,
        ODataValue::String("active".to_owned()),
    );
    let mut ctx = SqlCtx::new(1);
    let sql = translate_record_filter(&node, &mut ctx).unwrap();
    assert_eq!(sql, "status = $1");
    assert_eq!(ctx.binds.len(), 1);
    assert!(matches!(&ctx.binds[0], SqlBind::Str(s) if s == "active"));
}

#[test]
fn composite_and_renders_grouped_predicate_with_two_binds() {
    let node = FilterNode::Composite {
        op: FilterOp::And,
        children: vec![
            binary("tenant_id", FilterOp::Eq, uuid_val()),
            binary("created_at", FilterOp::Ge, dt_val()),
        ],
    };
    let mut ctx = SqlCtx::new(1);
    let sql = translate_record_filter(&node, &mut ctx).unwrap();
    assert_eq!(sql, "(tenant_id = $1 AND created_at >= $2)");
    assert_eq!(ctx.binds.len(), 2);
    assert!(matches!(&ctx.binds[0], SqlBind::Uuid(_)));
    assert!(matches!(&ctx.binds[1], SqlBind::DateTime(_)));
}

#[test]
fn in_list_renders_membership_with_one_placeholder_per_value() {
    let node = FilterNode::InList {
        field: rec_field("tenant_id"),
        values: vec![uuid_val(), uuid_val()],
    };
    let mut ctx = SqlCtx::new(1);
    let sql = translate_record_filter(&node, &mut ctx).unwrap();
    assert_eq!(sql, "tenant_id IN ($1, $2)");
    assert_eq!(ctx.binds.len(), 2);
}

#[test]
fn comparison_operators_render_their_exact_sql() {
    // `=` and `>=` are covered above; assert the remaining four comparison ops
    // emit the exact SQL operator (not just "translation succeeds").
    for (op, sql_op) in [
        (FilterOp::Ne, "<>"),
        (FilterOp::Gt, ">"),
        (FilterOp::Lt, "<"),
        (FilterOp::Le, "<="),
    ] {
        let node = binary("created_at", op, dt_val());
        let mut ctx = SqlCtx::new(1);
        let sql = translate_record_filter(&node, &mut ctx).unwrap();
        assert_eq!(sql, format!("created_at {sql_op} $1"), "op {op:?}");
    }
}

#[test]
fn composite_or_joins_children_with_or_inside_parens() {
    let node = FilterNode::Composite {
        op: FilterOp::Or,
        children: vec![
            binary(
                "status",
                FilterOp::Eq,
                ODataValue::String("active".to_owned()),
            ),
            binary(
                "status",
                FilterOp::Eq,
                ODataValue::String("inactive".to_owned()),
            ),
        ],
    };
    let mut ctx = SqlCtx::new(1);
    let sql = translate_record_filter(&node, &mut ctx).unwrap();
    assert_eq!(sql, "(status = $1 OR status = $2)");
    assert_eq!(ctx.binds.len(), 2);
}

#[test]
fn empty_in_list_is_rejected() {
    let node = FilterNode::InList {
        field: rec_field("tenant_id"),
        values: vec![],
    };
    let mut ctx = SqlCtx::new(1);
    let err = translate_record_filter(&node, &mut ctx).unwrap_err();
    assert!(err.contains("IN list must not be empty"), "got: {err}");
}

#[test]
fn not_wraps_inner_predicate() {
    let node = FilterNode::Not(Box::new(binary(
        "status",
        FilterOp::Eq,
        ODataValue::String("inactive".to_owned()),
    )));
    let mut ctx = SqlCtx::new(1);
    let sql = translate_record_filter(&node, &mut ctx).unwrap();
    assert_eq!(sql, "NOT (status = $1)");
}

#[test]
fn placeholder_numbering_honors_start_offset() {
    let node = binary(
        "status",
        FilterOp::Eq,
        ODataValue::String("active".to_owned()),
    );
    let mut ctx = SqlCtx::new(3);
    let sql = translate_record_filter(&node, &mut ctx).unwrap();
    assert_eq!(sql, "status = $3");
}

// `FilterField::from_name` resolves only schema fields, so an unmapped field
// is rejected at the AST boundary. We still guard the translator against a
// field whose `name()` is not on the column allowlist by exercising the
// usage-type column path against a record field that maps to no catalog
// column (`status`).
#[test]
fn record_filter_rejects_unmapped_field_against_catalog_allowlist() {
    let node = binary(
        "status",
        FilterOp::Eq,
        ODataValue::String("active".to_owned()),
    );
    let mut ctx = SqlCtx::new(1);
    let err = super::translate_usage_type_filter(&node, &mut ctx).unwrap_err();
    assert!(err.contains("not allowlisted"), "got: {err}");
}

#[test]
fn unsupported_operator_is_rejected() {
    let node = binary(
        "resource_id",
        FilterOp::Contains,
        ODataValue::String("vm".to_owned()),
    );
    let mut ctx = SqlCtx::new(1);
    assert!(translate_record_filter(&node, &mut ctx).is_err());
}

// A `Composite` node whose operator is neither `And` nor `Or` (only those two
// are joinable) must be rejected rather than emitting a bogus join. The parser
// never produces this shape, so it is a fail-closed guard on the translator's
// own invariant.
#[test]
fn composite_with_non_and_or_operator_is_rejected() {
    let node = FilterNode::Composite {
        op: FilterOp::Eq,
        children: vec![binary(
            "status",
            FilterOp::Eq,
            ODataValue::String("active".to_owned()),
        )],
    };
    let mut ctx = SqlCtx::new(1);
    let err = translate_record_filter(&node, &mut ctx).unwrap_err();
    assert!(err.contains("invalid composite operator"), "got: {err}");
}

// ── Order-by + keyset ────────────────────────────────────────────────────────

fn order_created_at_id() -> ODataOrderBy {
    ODataOrderBy(vec![
        OrderKey {
            field: "created_at".to_owned(),
            dir: SortDir::Asc,
        },
        OrderKey {
            field: "id".to_owned(),
            dir: SortDir::Asc,
        },
    ])
}

#[test]
fn render_order_by_renders_allowlisted_columns() {
    let sql = render_order_by(&order_created_at_id(), record_column).unwrap();
    assert_eq!(sql, "created_at ASC, id ASC");
}

#[test]
fn render_order_by_rejects_unknown_column() {
    let order = ODataOrderBy(vec![OrderKey {
        field: "not_a_column".to_owned(),
        dir: SortDir::Asc,
    }]);
    assert!(render_order_by(&order, record_column).is_err());
}

#[test]
fn render_order_by_rejects_empty_order() {
    let err = render_order_by(&ODataOrderBy(vec![]), record_column).unwrap_err();
    assert!(err.contains("order must not be empty"), "got: {err}");
}

#[test]
fn keyset_predicate_rejects_empty_order_pairs() {
    let pairs: &[(&str, bool)] = &[];
    let keys: Vec<String> = vec![];
    let mut ctx = SqlCtx::new(1);
    let err = keyset_predicate(
        pairs,
        &keys,
        record_column,
        rec_kind,
        rec_keyset_safe,
        &mut ctx,
    )
    .unwrap_err();
    assert!(err.contains("keyset order must not be empty"), "got: {err}");
}

#[test]
fn keyset_predicate_rejects_key_order_arity_mismatch() {
    // Two order pairs but a single cursor key: the tuple comparison would be
    // ill-formed, so it must fail closed rather than emit a truncated tuple.
    let pairs: &[(&str, bool)] = &[("created_at", true), ("id", true)];
    let keys = vec!["2026-01-02T03:04:05Z".to_owned()];
    let mut ctx = SqlCtx::new(1);
    let err = keyset_predicate(
        pairs,
        &keys,
        record_column,
        rec_kind,
        rec_keyset_safe,
        &mut ctx,
    )
    .unwrap_err();
    assert!(err.contains("does not match order arity"), "got: {err}");
}

#[test]
fn keyset_predicate_ascending_renders_tuple_comparison_with_two_binds() {
    let pairs: &[(&str, bool)] = &[("created_at", true), ("id", true)];
    let keys = vec![
        "2026-01-02T03:04:05Z".to_owned(),
        uuid::Uuid::from_u128(0x1234).to_string(),
    ];
    let mut ctx = SqlCtx::new(1);
    let sql = keyset_predicate(
        pairs,
        &keys,
        record_column,
        rec_kind,
        rec_keyset_safe,
        &mut ctx,
    )
    .unwrap();
    assert_eq!(sql, "(created_at, id) > ($1, $2)");
    assert_eq!(ctx.binds.len(), 2);
    assert!(matches!(&ctx.binds[0], SqlBind::DateTime(_)));
    assert!(matches!(&ctx.binds[1], SqlBind::Uuid(_)));
}

#[test]
fn keyset_predicate_descending_uses_less_than() {
    let pairs: &[(&str, bool)] = &[("created_at", false), ("id", false)];
    let keys = vec![
        "2026-01-02T03:04:05Z".to_owned(),
        uuid::Uuid::from_u128(0x1234).to_string(),
    ];
    let mut ctx = SqlCtx::new(1);
    let sql = keyset_predicate(
        pairs,
        &keys,
        record_column,
        rec_kind,
        rec_keyset_safe,
        &mut ctx,
    )
    .unwrap();
    assert_eq!(sql, "(created_at, id) < ($1, $2)");
}

#[test]
fn keyset_predicate_rejects_mixed_directions() {
    let pairs: &[(&str, bool)] = &[("created_at", true), ("id", false)];
    let keys = vec!["2026-01-02T03:04:05Z".to_owned(), "x".to_owned()];
    let mut ctx = SqlCtx::new(1);
    assert!(
        keyset_predicate(
            pairs,
            &keys,
            record_column,
            rec_kind,
            rec_keyset_safe,
            &mut ctx
        )
        .is_err()
    );
}

#[test]
fn keyset_predicate_rejects_a_nullable_ordering_column() {
    // Defence-in-depth backstop for finding #3: `subject_type` is an
    // allowlisted, kind-resolvable column, but it is nullable
    // (`subject_ref: Option<_>`). A row-value tuple keyset over it would
    // silently drop NULL-`subject_type` rows, so `keyset_predicate` must fail
    // closed rather than emit `(subject_type) > ($1)` — even though the gateway
    // already rejects such an `$orderby` with a 400, a crafted cursor could
    // still smuggle it in here.
    let pairs: &[(&str, bool)] = &[("subject_type", true)];
    let keys = vec!["vm".to_owned()];
    let mut ctx = SqlCtx::new(1);
    let err = keyset_predicate(
        pairs,
        &keys,
        record_column,
        rec_kind,
        rec_keyset_safe,
        &mut ctx,
    )
    .unwrap_err();
    assert!(
        err.contains("nullable") && err.contains("subject_type"),
        "error must name the nullable offending field; got: {err}"
    );
    assert!(ctx.binds.is_empty(), "no bind is pushed on the reject path");
}

#[test]
fn keyset_predicate_binds_uuid_column_as_uuid_not_text() {
    // `tenant_id` is a uuid column whose name is neither `id` nor
    // `corrects_id`. Typing the cursor key by column NAME (the old behaviour)
    // bound it as text, producing a `uuid > text` runtime error. Typing by the
    // field's declared `FieldKind` binds it as Uuid.
    let pairs: &[(&str, bool)] = &[("tenant_id", true)];
    let keys = vec![uuid::Uuid::from_u128(7).to_string()];
    let mut ctx = SqlCtx::new(1);
    let sql = keyset_predicate(
        pairs,
        &keys,
        record_column,
        rec_kind,
        rec_keyset_safe,
        &mut ctx,
    )
    .unwrap();
    assert_eq!(sql, "(tenant_id) > ($1)");
    assert!(
        matches!(ctx.binds[0], SqlBind::Uuid(_)),
        "uuid column binds as Uuid, got {:?}",
        ctx.binds[0]
    );
}

#[test]
fn cursor_key_to_bind_dispatches_on_field_kind() {
    assert!(matches!(
        cursor_key_to_bind(FieldKind::DateTimeUtc, "2026-01-02T03:04:05Z").unwrap(),
        SqlBind::DateTime(_)
    ));
    assert!(matches!(
        cursor_key_to_bind(FieldKind::Uuid, &uuid::Uuid::from_u128(1).to_string()).unwrap(),
        SqlBind::Uuid(_)
    ));
    assert!(matches!(
        cursor_key_to_bind(FieldKind::String, "active").unwrap(),
        SqlBind::Str(_)
    ));
    // Unparseable for its declared kind -> error.
    assert!(cursor_key_to_bind(FieldKind::DateTimeUtc, "not-a-date").is_err());
    assert!(cursor_key_to_bind(FieldKind::Uuid, "not-a-uuid").is_err());
    // A kind with no keyset binding fails closed rather than silently binding
    // as text.
    assert!(cursor_key_to_bind(FieldKind::Decimal, "1.5").is_err());
    assert!(cursor_key_to_bind(FieldKind::I64, "5").is_err());
}

#[test]
fn ensure_forward_cursor_rejects_backward_direction() {
    let mk = |d: &str| CursorV1 {
        k: vec!["x".to_owned()],
        o: SortDir::Asc,
        s: "+created_at".to_owned(),
        f: None,
        d: d.to_owned(),
    };
    assert!(
        ensure_forward_cursor(&mk("fwd")).is_ok(),
        "a forward cursor is accepted"
    );
    // The keyset comparison operator is derived from the sort direction, not
    // from `d`, so a backward cursor would silently be walked forward. It must
    // be rejected fail-closed until backward paging is actually implemented.
    assert!(
        ensure_forward_cursor(&mk("bwd")).is_err(),
        "a backward cursor is rejected"
    );
}

// ── Cursor round-trip ────────────────────────────────────────────────────────

#[test]
fn encode_then_decode_cursor_round_trips_keys_and_order() {
    let order = order_created_at_id();
    let keys = vec![
        "2026-01-02T03:04:05Z".to_owned(),
        uuid::Uuid::from_u128(0x1234).to_string(),
    ];
    let token = super::super::keyset::encode_next_cursor(&order, &keys, Some("hash")).unwrap();
    let decoded = super::super::keyset::decode_cursor(&token).unwrap();
    assert_eq!(decoded.k, keys);
    assert_eq!(decoded.s, "+created_at,+id");
    assert_eq!(decoded.d, "fwd");
    assert_eq!(decoded.f.as_deref(), Some("hash"));
}

#[test]
fn encode_next_cursor_rejects_row_key_order_arity_mismatch() {
    // A two-key order but a single last-row key: the cursor would encode fewer
    // keys than the order it claims to follow, so it must fail closed.
    let order = order_created_at_id();
    let keys = vec!["2026-01-02T03:04:05Z".to_owned()];
    let err = super::super::keyset::encode_next_cursor(&order, &keys, None).unwrap_err();
    assert!(err.contains("does not match order arity"), "got: {err}");
}

#[test]
fn decode_cursor_rejects_a_malformed_client_token() {
    // Cursor tokens are client-supplied and untrusted: a garbage token must
    // surface an error, never a partially-decoded `CursorV1` or a panic.
    assert!(super::super::keyset::decode_cursor("not-a-valid-cursor-token").is_err());
    assert!(super::super::keyset::decode_cursor("").is_err());
    // Valid base64url, but the decoded bytes are not a `CursorV1` JSON payload.
    assert!(super::super::keyset::decode_cursor("bm90LWpzb24").is_err());
}
