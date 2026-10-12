//! Handler-side unit tests for the user-ops REST surface.
//!
//! Scope: pin the [`super::lower_odata_to_list_users_query`] seam that
//! the `GET /tenants/{id}/users` handler uses to lower a raw
//! [`toolkit_odata::ODataQuery`] (parsed by the `OData` extractor) into
//! the SDK-side [`account_management_sdk::ListUsersQuery`]. The seam
//! lives in the handler gear on purpose: it is the one place where
//! boundary validation (unknown `$filter` field, type mismatches,
//! invalid pagination, cursor encoding) is converted to
//! [`crate::domain::error::DomainError::Validation`] before the
//! service layer runs.

use account_management_sdk::IdpUserFilterField;
use toolkit_odata::filter::{FilterNode, FilterOp, ODataValue};
use toolkit_odata::{ODataOrderBy, ODataQuery, OrderKey, SortDir};

use super::lower_odata_to_list_users_query;
use crate::domain::error::DomainError;

#[test]
fn lower_unfiltered_default_yields_default_pagination_no_filter_no_order() {
    let query = ODataQuery::default(); // empty $filter, empty $orderby
    let lowered = lower_odata_to_list_users_query(query, 200).expect("default lowers");
    assert_eq!(lowered.pagination.top(), 50);
    assert_eq!(lowered.pagination.cursor(), None);
    assert!(lowered.filter.is_none());
    assert!(lowered.order.is_none());
}

#[test]
fn lower_with_username_eq_filter_produces_typed_filter_node() {
    let query = ODataQuery {
        filter: Some(Box::new(toolkit_odata::ast::Expr::Compare(
            Box::new(toolkit_odata::ast::Expr::Identifier("username".into())),
            toolkit_odata::ast::CompareOperator::Eq,
            Box::new(toolkit_odata::ast::Expr::Value(
                toolkit_odata::ast::Value::String("alice".into()),
            )),
        ))),
        ..ODataQuery::default()
    };
    let lowered = lower_odata_to_list_users_query(query, 200).expect("lowers ok");
    assert!(matches!(
        lowered.filter,
        Some(FilterNode::Binary {
            field: IdpUserFilterField::Username,
            op: FilterOp::Eq,
            value: ODataValue::String(_),
        })
    ));
}

#[test]
fn lower_with_unknown_filter_field_returns_validation_error() {
    let query = ODataQuery {
        filter: Some(Box::new(toolkit_odata::ast::Expr::Compare(
            Box::new(toolkit_odata::ast::Expr::Identifier("foo".into())),
            toolkit_odata::ast::CompareOperator::Eq,
            Box::new(toolkit_odata::ast::Expr::Value(
                toolkit_odata::ast::Value::String("x".into()),
            )),
        ))),
        ..ODataQuery::default()
    };
    let err = lower_odata_to_list_users_query(query, 200).expect_err("unknown field must reject");
    let DomainError::Validation { detail } = err else {
        panic!("expected Validation, got {err:?}")
    };
    assert!(
        detail.contains("foo"),
        "error mentions the unknown field: {detail}"
    );
}

#[test]
fn lower_with_order_yields_some_order() {
    let query = ODataQuery {
        order: ODataOrderBy(vec![OrderKey {
            field: "last_name".into(),
            dir: SortDir::Asc,
        }]),
        ..ODataQuery::default()
    };
    let lowered = lower_odata_to_list_users_query(query, 200).expect("order lowers");
    let order = lowered.order.as_ref().expect("order forwarded");
    assert_eq!(order.0.len(), 1);
    assert_eq!(order.0[0].field, "last_name");
}

#[test]
fn lower_with_empty_order_yields_none() {
    let query = ODataQuery::default(); // empty ODataOrderBy
    let lowered = lower_odata_to_list_users_query(query, 200).expect("ok");
    assert!(
        lowered.order.is_none(),
        "empty $orderby lowers to None so service can inject default"
    );
}

#[test]
fn lower_clamps_caller_limit_to_max_top() {
    let query = ODataQuery {
        limit: Some(9_999), // beyond MAX_TOP=200
        ..ODataQuery::default()
    };
    let lowered = lower_odata_to_list_users_query(query, 200).expect("clamped lowers");
    assert_eq!(
        lowered.pagination.top(),
        200,
        "oversized limit clamps to max_top, not rejected"
    );
}

#[test]
fn lower_default_limit_when_caller_omits_it() {
    let query = ODataQuery::default();
    let lowered = lower_odata_to_list_users_query(query, 200).expect("ok");
    assert_eq!(lowered.pagination.top(), 50, "default 50 when limit absent");
}

#[test]
fn lower_with_string_value_on_uuid_field_rejects_validation() {
    // `id` is declared kind = "Uuid" on IdpUserFilterField; passing a
    // String value (`$filter=id eq 'abc'`) must surface as a 400 at
    // the seam, not as a plugin-side error.
    let query = ODataQuery {
        filter: Some(Box::new(toolkit_odata::ast::Expr::Compare(
            Box::new(toolkit_odata::ast::Expr::Identifier("id".into())),
            toolkit_odata::ast::CompareOperator::Eq,
            Box::new(toolkit_odata::ast::Expr::Value(
                toolkit_odata::ast::Value::String("abc".into()),
            )),
        ))),
        ..ODataQuery::default()
    };
    let err = lower_odata_to_list_users_query(query, 200).expect_err("string-on-uuid must reject");
    let DomainError::Validation { detail } = err else {
        panic!("expected Validation, got {err:?}")
    };
    assert!(
        detail.contains("id"),
        "error mentions the field with mismatched type: {detail}"
    );
}

#[test]
fn lower_recovers_order_from_cursor_when_caller_omits_orderby() {
    // Continuation requests: the OData extractor rejects `cursor +
    // $orderby` at parse time, so a caller paginating under a
    // non-default order arrives here with empty `query.order`.
    // The lowering MUST extract the order from the cursor's `s`
    // field rather than fall through to the service-side default —
    // otherwise the plugin's cursor-validation step trips an
    // OrderMismatch on every continuation under non-default order.
    use toolkit_odata::CursorV1;
    let cur = CursorV1 {
        k: vec!["alpha".to_owned()],
        o: SortDir::Desc,
        s: "-last_name,+id".to_owned(),
        f: None,
        d: "fwd".to_owned(),
    };
    let query = ODataQuery {
        cursor: Some(cur),
        ..ODataQuery::default()
    };
    let lowered = lower_odata_to_list_users_query(query, 200).expect("ok");
    let order = lowered
        .order
        .as_ref()
        .expect("order recovered from cursor.s");
    assert_eq!(order.0.len(), 2);
    assert_eq!(order.0[0].field, "last_name");
    assert_eq!(order.0[0].dir, SortDir::Desc);
    assert_eq!(order.0[1].field, "id");
    assert_eq!(order.0[1].dir, SortDir::Asc);
}

#[test]
fn lower_caller_orderby_overrides_cursor_order_when_both_present() {
    // Practically the OData extractor rejects `cursor + $orderby`,
    // but pin the lowering's preference order anyway: an explicit
    // caller `$orderby` wins over the cursor-recovered one. This
    // documents the precedence even if the upper layer prevents
    // the combination in normal flows.
    use toolkit_odata::CursorV1;
    let cur = CursorV1 {
        k: vec!["alpha".to_owned()],
        o: SortDir::Asc,
        s: "+last_name,+id".to_owned(),
        f: None,
        d: "fwd".to_owned(),
    };
    let query = ODataQuery {
        cursor: Some(cur),
        order: ODataOrderBy(vec![OrderKey {
            field: "first_name".to_owned(),
            dir: SortDir::Asc,
        }]),
        ..ODataQuery::default()
    };
    let lowered = lower_odata_to_list_users_query(query, 200).expect("ok");
    let order = lowered.order.as_ref().expect("order forwarded");
    assert_eq!(
        order.0.len(),
        1,
        "caller-supplied $orderby wins over cursor"
    );
    assert_eq!(order.0[0].field, "first_name");
}

#[test]
fn lower_with_unknown_orderby_field_returns_validation_error() {
    // OData extractor doesn't whitelist orderby field names at parse
    // time — the lowering seam MUST gate them against IdpUserFilterField,
    // otherwise an unknown field silently no-ops at the plugin layer
    // (the route's with_odata_orderby helper only advertises the
    // allowed set on the OpenAPI surface, it does not enforce).
    let query = ODataQuery {
        order: ODataOrderBy(vec![OrderKey {
            field: "foo".into(),
            dir: SortDir::Asc,
        }]),
        ..ODataQuery::default()
    };
    let err =
        lower_odata_to_list_users_query(query, 200).expect_err("unknown $orderby MUST reject");
    let DomainError::Validation { detail } = err else {
        panic!("expected Validation, got {err:?}")
    };
    assert!(
        detail.contains("foo") && detail.contains("$orderby"),
        "error mentions the bad field and $orderby: {detail}"
    );
}

#[test]
fn lower_with_max_top_zero_floors_to_one_no_panic() {
    // Defensive: a deployment misconfigured with `max_listing_top = 0`
    // must NOT panic inside `clamp(1, 0)`. The seam floors max_top to 1
    // and falls back to the SDK MAX_TOP cap via subsequent clamps.
    let query = ODataQuery {
        limit: Some(10),
        ..ODataQuery::default()
    };
    let lowered =
        lower_odata_to_list_users_query(query, 0).expect("max_top=0 must not panic the seam");
    assert!(
        lowered.pagination.top() >= 1,
        "floored top must satisfy IdpUserPagination::top >= 1 invariant"
    );
}

/// A cursor whose sort order names a field outside the whitelist is refused like an unknown `$orderby`.
#[test]
fn lower_with_unknown_field_in_cursor_order_returns_validation_error() {
    // The cursor is caller-held: its `s` order passes the same
    // whitelist as `$orderby`, so a forged unknown field never reaches
    // the plugin (the static plugin has no projection for one).
    use toolkit_odata::CursorV1;
    let cur = CursorV1 {
        k: vec!["alpha".to_owned(), "id".to_owned()],
        o: SortDir::Asc,
        s: "+foo,+id".to_owned(),
        f: None,
        d: "fwd".to_owned(),
    };
    let query = ODataQuery {
        cursor: Some(cur),
        ..ODataQuery::default()
    };
    let err = lower_odata_to_list_users_query(query, 200)
        .expect_err("an unknown field in the cursor's order must reject");
    let DomainError::Validation { detail } = err else {
        panic!("expected Validation, got {err:?}")
    };
    assert!(detail.contains("`foo`"), "names the field: {detail}");
}
