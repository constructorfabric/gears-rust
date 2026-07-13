//! SDK-level unit tests for the user-operations contract types.
//!
//! Cover the small surface owned by the SDK alone: constructor
//! invariants, metric-label stability, and serde round-trips on the
//! published projection / payload shapes. Plugin behaviour is tested
//! at the impl-side seams (AM `UserService` against
//! `FakeIdpUserProvisioner`).

#![allow(clippy::expect_used, clippy::unwrap_used, reason = "test helpers")]

use super::*;
use toolkit_gts::gts_id;

#[test]
fn tenant_context_new_carries_inputs_verbatim() {
    let id = Uuid::from_u128(0x42);
    let tenant_type =
        gts::GtsTypeId::new(gts_id!("cf.core.am.tenant_type.v1~cf.core.am.customer.v1~"));
    let ctx = IdpTenantContext::new(id, "acme", tenant_type.clone(), None);
    assert_eq!(ctx.tenant_id, id);
    assert_eq!(ctx.tenant_name, "acme");
    assert_eq!(ctx.tenant_type, tenant_type);
    assert!(ctx.metadata.is_none());
}

#[test]
fn tenant_context_new_with_metadata_populates_field() {
    let id = Uuid::from_u128(0x43);
    let tenant_type =
        gts::GtsTypeId::new(gts_id!("cf.core.am.tenant_type.v1~cf.core.am.customer.v1~"));
    let metadata = serde_json::json!({"realm": "acme-prod"});
    let ctx = IdpTenantContext::new(id, "acme", tenant_type.clone(), Some(metadata.clone()));
    assert_eq!(ctx.tenant_type, tenant_type);
    assert_eq!(ctx.metadata.as_ref(), Some(&metadata));
}

#[test]
fn tenant_context_serde_skips_absent_metadata() {
    // `metadata = None` is the default-and-most-common shape for
    // plugins that bind via external configuration; the wire payload
    // stays minimal in that case.
    let tenant_type = gts::GtsTypeId::new(gts_id!("cf.core.am.tenant_type.v1~cf.core.am.x.v1~"));
    let ctx = IdpTenantContext::new(Uuid::from_u128(0x44), "acme", tenant_type.clone(), None);
    let json = serde_json::to_value(&ctx).expect("serialise");
    let obj = json.as_object().expect("object");
    assert!(obj.contains_key("tenant_id"));
    assert!(obj.contains_key("tenant_name"));
    assert!(obj.contains_key("tenant_type"));
    assert!(
        !obj.contains_key("metadata"),
        "absent metadata MUST NOT appear on the wire"
    );

    let with_metadata = IdpTenantContext::new(
        Uuid::from_u128(0x44),
        "acme",
        tenant_type,
        Some(serde_json::json!({"realm": "x"})),
    );
    let json = serde_json::to_value(&with_metadata).expect("serialise");
    assert!(
        json.get("metadata").is_some(),
        "populated metadata MUST surface on the wire"
    );
}

#[test]
fn user_operation_failure_metric_labels_are_stable() {
    assert_eq!(
        IdpUserOperationFailure::Unavailable { detail: "x".into() }.as_metric_label(),
        "unavailable"
    );
    assert_eq!(
        IdpUserOperationFailure::UnsupportedOperation { detail: "x".into() }.as_metric_label(),
        "unsupported_operation"
    );
    assert_eq!(
        IdpUserOperationFailure::Rejected { detail: "x".into() }.as_metric_label(),
        "rejected"
    );
}

#[test]
fn user_operation_failure_detail_and_display() {
    let f = IdpUserOperationFailure::Unavailable {
        detail: "timeout".into(),
    };
    assert_eq!(f.detail(), "timeout");
    // Same `"<metric_label>: <detail>"` shape as the sibling `IdP`
    // failure enums in `crate::idp` so audit / structured-log
    // consumers see a uniform format across tenant and user ops.
    assert_eq!(f.to_string(), "unavailable: timeout");
    let f2 = IdpUserOperationFailure::Rejected {
        detail: "dup username".into(),
    };
    assert_eq!(f2.to_string(), "rejected: dup username");
}

#[test]
fn user_operation_failure_implements_std_error_trait() {
    let f = IdpUserOperationFailure::UnsupportedOperation { detail: "x".into() };
    let _: &dyn core::error::Error = &f;
}

#[test]
fn user_pagination_new_rejects_zero_top() {
    assert_eq!(
        IdpUserPagination::new(0, None).unwrap_err(),
        IdpUserPaginationError::TopMustBePositive
    );
    let valid =
        IdpUserPagination::new(25, Some("opaque-cursor".to_owned())).expect("top=25 is valid");
    assert_eq!(valid.top(), 25);
    assert_eq!(valid.cursor(), Some("opaque-cursor"));
}

#[test]
fn user_pagination_new_rejects_top_above_max() {
    // `top` exactly at the cap is accepted.
    let at_cap = IdpUserPagination::new(IdpUserPagination::MAX_TOP, None)
        .expect("top == MAX_TOP must be accepted");
    assert_eq!(at_cap.top(), IdpUserPagination::MAX_TOP);

    // `top` one past the cap is rejected with the structured error
    // (caller can format `requested` / `max` for the audit envelope).
    assert_eq!(
        IdpUserPagination::new(IdpUserPagination::MAX_TOP + 1, None).unwrap_err(),
        IdpUserPaginationError::TopExceedsMax {
            requested: IdpUserPagination::MAX_TOP + 1,
            max: IdpUserPagination::MAX_TOP
        }
    );

    // `u32::MAX` is the realistic abuse case — a caller forwarding an
    // unvalidated wire value MUST NOT reach the `IdP` plugin layer.
    assert_eq!(
        IdpUserPagination::new(u32::MAX, None).unwrap_err(),
        IdpUserPaginationError::TopExceedsMax {
            requested: u32::MAX,
            max: IdpUserPagination::MAX_TOP
        }
    );
}

#[test]
fn user_pagination_new_rejects_oversized_cursor() {
    let huge = "x".repeat(IdpUserPagination::MAX_CURSOR_LEN + 1);
    let len = huge.len();
    assert_eq!(
        IdpUserPagination::new(10, Some(huge)).unwrap_err(),
        IdpUserPaginationError::CursorTooLong {
            len,
            max: IdpUserPagination::MAX_CURSOR_LEN
        }
    );

    // Exactly at the cap is accepted — defensive symmetry with the
    // MAX_TOP boundary above.
    let at_cap = "y".repeat(IdpUserPagination::MAX_CURSOR_LEN);
    let ok = IdpUserPagination::new(10, Some(at_cap))
        .expect("cursor length == MAX_CURSOR_LEN must be accepted");
    assert_eq!(
        ok.cursor().map(str::len),
        Some(IdpUserPagination::MAX_CURSOR_LEN)
    );
}

#[test]
fn user_pagination_default_uses_default_top_not_zero() {
    let p = IdpUserPagination::default();
    assert_eq!(p.top(), IdpUserPagination::DEFAULT_TOP);
    assert_eq!(p.cursor(), None);
    assert!(
        p.top() > 0,
        "Default::default() MUST NOT yield top=0 (would silently empty list_users \
         existence checks for providers that honor literal 0)"
    );
}

#[test]
fn user_pagination_deserialize_uses_default_top_when_absent() {
    // Wire payload omits `top` (a continuation request that carries
    // only the opaque cursor). Without `#[serde(default = ...)]` on
    // `RawUserPagination::top`, this would fail deserialization with
    // "missing field `top`" — contradicting the documented default.
    let only_cursor = serde_json::json!({"cursor": "abc-token"});
    let parsed: IdpUserPagination =
        serde_json::from_value(only_cursor).expect("missing top must use the documented default");
    assert_eq!(parsed.top(), IdpUserPagination::DEFAULT_TOP);
    assert_eq!(parsed.cursor(), Some("abc-token"));

    let empty = serde_json::json!({});
    let parsed: IdpUserPagination =
        serde_json::from_value(empty).expect("empty object must use both defaults");
    assert_eq!(parsed.top(), IdpUserPagination::DEFAULT_TOP);
    assert_eq!(parsed.cursor(), None);
}

#[test]
fn user_pagination_deserialize_rejects_zero_top() {
    // The wire path (REST query string, plugin RPC, etc.) routes
    // through `RawUserPagination` + `TryFrom` so the same `top > 0`
    // invariant is enforced on every deserialisation input.
    let bad = serde_json::json!({"top": 0});
    assert!(
        serde_json::from_value::<IdpUserPagination>(bad).is_err(),
        "top=0 MUST fail to deserialise"
    );
    let good = serde_json::json!({"top": 10, "cursor": "next-page-token"});
    let parsed: IdpUserPagination = serde_json::from_value(good).expect("top=10 is valid");
    assert_eq!(parsed.top(), 10);
    assert_eq!(parsed.cursor(), Some("next-page-token"));
}

#[test]
fn new_user_payload_serde_skips_absent_optionals() {
    let payload = IdpNewUser::new("bob");
    let json = serde_json::to_value(&payload).expect("serialise");
    let map = json.as_object().expect("json object");
    assert!(map.contains_key("username"));
    assert!(!map.contains_key("email"));
    assert!(!map.contains_key("display_name"));
    assert!(!map.contains_key("first_name"));
    assert!(!map.contains_key("last_name"));
    assert!(!map.contains_key("password"));
}

#[test]
fn new_user_payload_builders_populate_all_optionals() {
    // Builder methods stay additive — chaining first/last/password
    // does not erase earlier email/display-name. Plugins that bind to
    // the granular fields rely on first_name/last_name being preserved
    // alongside display_name (the SDK doc-comment commits to that).
    let payload = IdpNewUser::new("alice")
        .with_email("alice@example.test")
        .with_display_name("Alice A")
        .with_first_name("Alice")
        .with_last_name("Anderson")
        .with_password("s3cret!", true);
    assert_eq!(payload.username, "alice");
    assert_eq!(payload.email.as_deref(), Some("alice@example.test"));
    assert_eq!(payload.display_name.as_deref(), Some("Alice A"));
    assert_eq!(payload.first_name.as_deref(), Some("Alice"));
    assert_eq!(payload.last_name.as_deref(), Some("Anderson"));
    let pw = payload.password.as_ref().expect("password set");
    assert_eq!(pw.value, "s3cret!");
    assert!(
        pw.temporary,
        "temporary flag must round-trip the builder arg"
    );
}

#[test]
fn new_user_payload_with_password_permanent_default() {
    // `temporary = false` is the "permanent credential" arm — password
    // grants succeed without going through `UPDATE_PASSWORD`. The
    // builder MUST honour the caller's boolean verbatim (no implicit
    // policy override) so callers can construct either shape.
    let payload = IdpNewUser::new("bob").with_password("hunter2", false);
    let pw = payload.password.as_ref().expect("password set");
    assert_eq!(pw.value, "hunter2");
    assert!(!pw.temporary);
}

#[test]
fn new_user_password_debug_redacts_value_but_preserves_temporary() {
    // Debug is the leak channel we care about: structured logs and
    // `tracing::debug!(?req)` will format the field via this impl. The
    // value MUST NOT appear; the `temporary` flag is non-sensitive
    // and stays visible for diagnostics.
    let pw = NewUserPassword {
        value: "super-secret-password".into(),
        temporary: true,
    };
    let dbg = format!("{pw:?}");
    assert!(
        !dbg.contains("super-secret-password"),
        "plaintext password leaked into Debug: `{dbg}`"
    );
    assert!(
        dbg.contains("<redacted>"),
        "Debug must mark the redaction explicitly: `{dbg}`"
    );
    assert!(
        dbg.contains("temporary: true"),
        "non-sensitive temporary flag must remain visible: `{dbg}`"
    );
}

#[test]
fn new_user_payload_debug_does_not_leak_password() {
    // The IdpNewUser Debug impl is hand-rolled; the test pins the
    // nested-redaction guarantee so a future field addition cannot
    // accidentally fall back to `derive(Debug)` (which would still
    // delegate to the redacted `NewUserPassword::Debug`, but only as
    // long as that nested impl is wired through — easy to break).
    let payload = IdpNewUser::new("alice")
        .with_email("alice@example.test")
        .with_password("super-secret-password", false);
    let dbg = format!("{payload:?}");
    assert!(
        !dbg.contains("super-secret-password"),
        "plaintext password leaked from IdpNewUser Debug: `{dbg}`"
    );
    assert!(
        dbg.contains("<redacted>"),
        "IdpNewUser Debug must mark the password redaction: `{dbg}`"
    );
    assert!(
        dbg.contains("alice@example.test"),
        "non-sensitive email must remain visible: `{dbg}`"
    );
}

#[test]
fn new_user_payload_serde_round_trips_full_payload() {
    // Round-trip pins the wire shape: the field names plugins read
    // (`first_name`, `last_name`, `password.value`, `password.temporary`)
    // MUST stay literally these. A rename would break every IdP plugin
    // that pattern-matches the JSON shape.
    let payload = IdpNewUser::new("alice")
        .with_email("alice@example.test")
        .with_first_name("Alice")
        .with_last_name("Anderson")
        .with_password("s3cret!", true);
    let json = serde_json::to_value(&payload).expect("serialise");
    assert_eq!(
        json,
        serde_json::json!({
            "username": "alice",
            "email": "alice@example.test",
            "first_name": "Alice",
            "last_name": "Anderson",
            "password": {"value": "s3cret!", "temporary": true},
        })
    );

    let parsed: IdpNewUser = serde_json::from_value(json).expect("deserialise");
    assert_eq!(parsed.first_name.as_deref(), Some("Alice"));
    assert_eq!(parsed.last_name.as_deref(), Some("Anderson"));
    let pw = parsed.password.as_ref().expect("password set");
    assert_eq!(pw.value, "s3cret!");
    assert!(pw.temporary);
}

#[test]
fn new_user_password_deserialise_defaults_temporary_to_false() {
    // `temporary` carries `#[serde(default)]` so callers can omit it
    // for the common permanent-credential shape. Without the default
    // the wire payload would have to carry `"temporary": false`
    // explicitly, which would surprise integrations.
    let parsed: NewUserPassword =
        serde_json::from_value(serde_json::json!({"value": "hunter2"})).expect("deserialise");
    assert_eq!(parsed.value, "hunter2");
    assert!(
        !parsed.temporary,
        "missing `temporary` MUST default to false (permanent credential)"
    );
}

#[test]
fn idp_user_first_last_name_round_trip_through_serde() {
    let id = Uuid::from_u128(0x00A1_10CE);
    let user = IdpUser::new(id, "alice")
        .with_first_name("Alice")
        .with_last_name("Anderson");
    assert_eq!(user.first_name.as_deref(), Some("Alice"));
    assert_eq!(user.last_name.as_deref(), Some("Anderson"));

    let json = serde_json::to_value(&user).expect("serialise");
    assert_eq!(json["first_name"], "Alice");
    assert_eq!(json["last_name"], "Anderson");

    let back: IdpUser = serde_json::from_value(json).expect("deserialise");
    assert_eq!(back.first_name.as_deref(), Some("Alice"));
    assert_eq!(back.last_name.as_deref(), Some("Anderson"));
}

#[test]
fn idp_user_absent_first_last_name_drops_keys_on_wire() {
    let user = IdpUser::new(Uuid::from_u128(0x00A1_10CE), "alice");
    let json = serde_json::to_value(&user).expect("serialise");
    let map = json.as_object().expect("object");
    assert!(!map.contains_key("first_name"));
    assert!(!map.contains_key("last_name"));
}

#[test]
fn idp_user_filter_field_set_is_pinned() {
    use toolkit_odata::filter::FilterField;
    let names: Vec<&'static str> = IdpUserFilterField::FIELDS
        .iter()
        .map(toolkit_odata::filter::FilterField::name)
        .collect();
    assert_eq!(
        names,
        vec![
            "id",
            "username",
            "email",
            "display_name",
            "first_name",
            "last_name",
        ],
        "IdpUserFilterField columns AND their declaration order must stay locked - a rename, removal, addition, or reorder here is breaking",
    );
}

#[test]
fn idp_list_users_request_carries_typed_filter_and_order() {
    use toolkit_odata::filter::{FilterNode, FilterOp, ODataValue};
    use toolkit_odata::{ODataOrderBy, OrderKey, SortDir};

    let ctx = IdpTenantContext::new(
        Uuid::from_u128(1),
        "acme",
        gts::GtsTypeId::new(gts_id!("cf.core.am.tenant_type.v1~cf.core.am.customer.v1~")),
        None,
    );
    let pagination = IdpUserPagination::default();
    let filter = FilterNode::binary(
        IdpUserFilterField::Username,
        FilterOp::Eq,
        ODataValue::String("alice".into()),
    );
    let order = ODataOrderBy(vec![OrderKey {
        field: "username".into(),
        dir: SortDir::Asc,
    }]);

    let req = IdpListUsersRequest::new(ctx, pagination)
        .with_filter(filter)
        .with_order(order);

    assert!(matches!(
        req.filter,
        Some(FilterNode::Binary {
            field: IdpUserFilterField::Username,
            op: FilterOp::Eq,
            ..
        })
    ));
    assert_eq!(req.order.as_ref().expect("order set").0.len(), 1);
}

#[test]
fn idp_list_users_request_new_defaults_filter_and_order_to_none() {
    let ctx = IdpTenantContext::new(
        Uuid::from_u128(2),
        "acme",
        gts::GtsTypeId::new(gts_id!("cf.core.am.tenant_type.v1~cf.core.am.customer.v1~")),
        None,
    );
    let req = IdpListUsersRequest::new(ctx, IdpUserPagination::default());
    assert!(req.filter.is_none(), "new() must leave filter unset");
    assert!(req.order.is_none(), "new() must leave order unset");
}

#[test]
fn list_users_query_with_id_helper_builds_eq_filter_and_pins_top_one() {
    use toolkit_odata::filter::ODataValue;
    use toolkit_odata::filter::{FilterNode, FilterOp};

    let target = Uuid::from_u128(0xCAFE);
    let query = ListUsersQuery::with_id(target);
    assert_eq!(query.pagination.top(), 1);
    assert!(query.pagination.cursor().is_none());
    let filter = query.filter.as_ref().expect("filter present");
    match filter {
        FilterNode::Binary { field, op, value } => {
            assert!(matches!(field, IdpUserFilterField::Id));
            assert!(matches!(op, FilterOp::Eq));
            match value {
                ODataValue::Uuid(u) => assert_eq!(*u, target),
                other => panic!("expected Uuid value, got {other:?}"),
            }
        }
        other => panic!("expected Binary node, got {other:?}"),
    }
    assert!(query.order.is_none(), "with_id MUST NOT preset an order");
}

#[test]
fn list_users_query_default_carries_no_filter_or_order() {
    let q = ListUsersQuery::default();
    assert!(q.filter.is_none());
    assert!(q.order.is_none());
}

#[test]
fn list_users_query_with_filter_and_order_builders_round_trip() {
    use toolkit_odata::filter::{FilterNode, FilterOp, ODataValue};
    use toolkit_odata::{ODataOrderBy, OrderKey, SortDir};

    let filter = FilterNode::binary(
        IdpUserFilterField::Email,
        FilterOp::Eq,
        ODataValue::String("alice@example.test".into()),
    );
    let order = ODataOrderBy(vec![OrderKey {
        field: "username".into(),
        dir: SortDir::Asc,
    }]);
    let q = ListUsersQuery::new(IdpUserPagination::default())
        .with_filter(filter)
        .with_order(order);
    assert!(matches!(
        q.filter,
        Some(FilterNode::Binary {
            field: IdpUserFilterField::Email,
            op: FilterOp::Eq,
            ..
        })
    ));
    assert_eq!(q.order.as_ref().expect("order set").0.len(), 1);
}
