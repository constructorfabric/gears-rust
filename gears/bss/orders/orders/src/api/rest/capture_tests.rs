#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code: a failed setup or fixture must stop the test at once; `allow`, not \
              `expect`, because not every test in the file unwraps or expects"
)]
use super::*;
use crate::api::rest::dto;
use crate::domain::capture::wire_fields;
use crate::domain::idempotency::StoredResponse;
use crate::infra::capture::SettledOutcome;
use bss_orders_sdk::authoring::{
    AuthoredTerm, BillingCycle, CalendarDate, Category, Currency, DraftLine, OrderHeader,
    OrderView, SalesPath, SelectedItem, VersionSummary,
};
use bss_orders_sdk::catalog::OrderState;
use bss_orders_sdk::models::{OrderVersion, TransitionResult};
use serde_json::json;

fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
    let mut h = HeaderMap::new();
    for (k, v) in pairs {
        h.append(
            HeaderName::try_from(*k).unwrap(),
            HeaderValue::from_str(v).unwrap(),
        );
    }
    h
}
fn reason(e: OrdersError) -> Reason {
    match e {
        OrdersError::Refused(r) => r,
        other => panic!("{other:?}"),
    }
}

#[test]
fn write_metadata_follows_the_catalog_boundary_order() {
    // Missing/weak/repeated If-Match is 428 before the key or proof is examined.
    for h in [
        headers(&[("idempotency-key", "")]),
        headers(&[("if-match", "W/\"1\""), ("x-delegation-proof-ref", "a b")]),
        headers(&[("if-match", "\"1\""), ("if-match", "\"2\"")]),
        headers(&[("if-match", "\"0\"")]),
    ] {
        assert_eq!(
            reason(write_meta(&h).unwrap_err()),
            Reason::ExpectedVersionRequired
        );
    }
    // Then the key, then the proof header.
    let h = headers(&[("if-match", "\"1\""), ("x-delegation-proof-ref", "a b")]);
    assert_eq!(reason(write_meta(&h).unwrap_err()), Reason::RequestInvalid);
    let h = headers(&[
        ("if-match", "\"1\""),
        ("idempotency-key", "k"),
        ("x-delegation-proof-ref", "a b"),
    ]);
    assert_eq!(reason(write_meta(&h).unwrap_err()), Reason::RequestInvalid);
    let h = headers(&[
        ("if-match", "\"7\""),
        ("idempotency-key", "k 1"),
        ("x-delegation-proof-ref", "proof-1"),
    ]);
    let meta = write_meta(&h).unwrap();
    assert_eq!(i64::from(meta.expected_version), 7);
    assert_eq!(String::from(meta.idempotency_key), "k 1");
    assert_eq!(meta.delegation_proof_ref.unwrap().as_str(), "proof-1");
}

#[test]
fn the_draft_revision_is_an_optional_nonnegative_integer_member() {
    let take = |v: Value| {
        let mut map = v.as_object().unwrap().clone();
        take_draft_revision("test", &mut map).map(|r| (r.map(i64::from), map.len()))
    };
    assert_eq!(take(json!({"fields": {}})).unwrap(), (None, 1));
    assert_eq!(
        take(json!({"expected_draft_revision": 0, "fields": {}})).unwrap(),
        (Some(0), 1)
    );
    for bad in [json!(true), json!(-1), json!("1"), json!(1.5), json!(null)] {
        assert_eq!(
            reason(take(json!({ "expected_draft_revision": bad })).unwrap_err()),
            Reason::RequestInvalid
        );
    }
}

#[test]
fn patch_bodies_name_fields_exactly() {
    let ok = |v: Value| fields("test", v.as_object().unwrap().clone());
    assert!(ok(json!({"fields": {"display_label": "x"}})).is_ok());
    for bad in [
        json!({"fields": {}}),
        json!({"fields": []}),
        json!({}),
        json!({"fields": {"display_label": "x"}, "actor": "forged"}),
    ] {
        assert_eq!(reason(ok(bad).unwrap_err()), Reason::RequestInvalid);
    }
    // Unknown, read-through and null-for-required fields are boundary-invalid.
    for bad in [
        json!({"discount_override": 5}),
        json!({"auto_renewal": true}),
        json!({"payer_tenant_id": null}),
        json!({"category": "gts.cf.bss.orders.category.v1~cf.bss.orders.other.v1"}),
    ] {
        assert!(decode::<dto::OrdersHeaderFields, HeaderPatch>("test", bad).is_err());
    }
    // Admin text caps count Unicode scalar values and reject NUL.
    let label = |s: String| {
        decode::<dto::OrdersHeaderFields, HeaderPatch>("test", json!({ "display_label": s }))
            .unwrap()
    };
    assert!(label("\u{e9}".repeat(200)).validate().is_ok());
    assert!(label("\u{e9}".repeat(201)).validate().is_err());
    assert!(label("a\0b".into()).validate().is_err());
    assert!(
        decode::<dto::OrdersLineFields, LinePatch>(
            "test",
            json!({"internal_notes": "n".repeat(2001)})
        )
        .unwrap()
        .validate()
        .is_err()
    );
    // Create: no actor override, no client order ID.
    let create = json!({
        "resource_tenant_id": Uuid::nil(), "payer_tenant_id": Uuid::nil(),
        "seller_tenant_id": Uuid::nil(), "category": Category::NEW_SALE,
    });
    assert!(decode::<dto::OrdersCreateOrder, CreateOrder>("test", create.clone()).is_ok());
    for extra in ["actor", "order_id", "order_number", "sales_path"] {
        let mut body = create.clone();
        body[extra] = json!("forged");
        assert!(
            decode::<dto::OrdersCreateOrder, CreateOrder>("test", body).is_err(),
            "{extra}"
        );
    }
    // A line insert takes no external reference (administrative, D-118).
    let line = json!({
        "plan_id": Uuid::nil(), "plan_revision_id": Uuid::nil(), "selected_items": [],
        "currency": "EUR", "external_reference": "PO-1",
    });
    assert!(decode::<dto::OrdersAddLine, AddLine>("test", line).is_err());
}

fn sample_line() -> AddLine {
    AddLine {
        plan_id: Uuid::from_u128(1),
        plan_revision_id: Uuid::from_u128(2),
        selected_items: vec![SelectedItem {
            item_id: Uuid::from_u128(3),
            quantity: Some(serde_json::from_value(json!("2.5")).unwrap()),
            selected_dim_value: Some("eu-west".into()),
        }],
        currency: Currency::try_from("EUR".to_owned()).unwrap(),
        contract_effective_date: Some(CalendarDate(
            time::Date::from_calendar_date(2026, time::Month::November, 1).unwrap(),
        )),
        service_activation_date: None,
        acceptance_due_date: None,
        term_duration: Some(AuthoredTerm::Calendar {
            years: 1,
            months: 0,
            days: 0,
            microseconds: 0,
        }),
        billing_cycle: Some(BillingCycle::Month),
    }
}

/// The wire DTOs accept exactly the SDK serialization and render it unchanged.
fn pinned<D: serde::de::DeserializeOwned + serde::Serialize>(sdk: &impl serde::Serialize) {
    let wire = serde_json::to_value(sdk).unwrap();
    let mirror: D = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(serde_json::to_value(mirror).unwrap(), wire);
}

#[test]
fn openapi_mirrors_are_pinned_to_the_sdk_wire() {
    let t0 = time::OffsetDateTime::from_unix_timestamp_nanos(1_791_288_000_123_456_000).unwrap();
    pinned::<dto::OrdersCreateOrder>(&CreateOrder {
        resource_tenant_id: Uuid::from_u128(10),
        payer_tenant_id: Uuid::from_u128(30),
        seller_tenant_id: Uuid::from_u128(20),
        category: Category::NewSale,
        contract_id: None,
    });
    pinned::<dto::OrdersAddLine>(&sample_line());
    for term in [
        AuthoredTerm::Rolling,
        AuthoredTerm::Periods { count: 12 },
        AuthoredTerm::Calendar {
            years: 0,
            months: 1,
            days: 2,
            microseconds: 3,
        },
    ] {
        pinned::<dto::OrdersAuthoredTerm>(&term);
    }
    pinned::<dto::OrdersHeaderFields>(&HeaderPatch {
        payer_tenant_id: Some(Uuid::from_u128(31)),
        contract_id: Some(None),
        external_reference: Some(Some("PO".into())),
        ..HeaderPatch::default()
    });
    pinned::<dto::OrdersLineFields>(&LinePatch {
        currency: Some(Currency::try_from("USD".to_owned()).unwrap()),
        term_duration: Some(None),
        billing_cycle: Some(Some(BillingCycle::Year)),
        ..LinePatch::default()
    });
    pinned::<dto::OrdersTransitionResult>(&TransitionResult {
        order_id: Uuid::from_u128(1),
        state: OrderState::Draft,
        version: OrderVersion::try_from(1).unwrap(),
        draft_revision: Some(DraftRevision::try_from(3).unwrap()),
        request_audit_id: None,
        line_id: Some(Uuid::from_u128(5)),
    });
    let line = sample_line();
    pinned::<dto::OrdersOrderView>(&OrderView {
        order: OrderHeader {
            order_id: Uuid::from_u128(1),
            order_number: "ORD-1".into(),
            state: OrderState::Draft,
            category: Category::NewSale,
            resource_tenant_id: Uuid::from_u128(10),
            seller_tenant_id: Uuid::from_u128(20),
            payer_tenant_id: Uuid::from_u128(30),
            contract_id: Some(Uuid::from_u128(40)),
            sales_path: SalesPath::SelfService,
            created_at: t0,
            state_entered_at: t0,
        },
        version: VersionSummary {
            version: OrderVersion::try_from(1).unwrap(),
            supersedes_version: None,
            created_at: t0,
        },
        lines: vec![DraftLine {
            line_id: Uuid::from_u128(9),
            plan_id: line.plan_id,
            plan_revision_id: line.plan_revision_id,
            selected_items: line.selected_items,
            currency: line.currency,
            contract_effective_date: line.contract_effective_date,
            service_activation_date: None,
            acceptance_due_date: None,
            term_duration: line.term_duration,
            billing_cycle: line.billing_cycle,
        }],
        draft_revision: Some(DraftRevision::try_from(0).unwrap()),
    });
    // Every response body name is snake_case (D-206); `draftRevision` is gone.
    let body = serde_json::to_value(TransitionResult {
        order_id: Uuid::nil(),
        state: OrderState::Draft,
        version: OrderVersion::try_from(1).unwrap(),
        draft_revision: Some(DraftRevision::try_from(0).unwrap()),
        request_audit_id: None,
        line_id: None,
    })
    .unwrap();
    assert_eq!(body["draft_revision"], json!(0));
    assert!(body.get("draftRevision").is_none());
}

/// The `OpenAPI` property names a mirror publishes, sorted.
fn schema_properties<D: utoipa::PartialSchema>() -> Vec<String> {
    let schema = serde_json::to_value(D::schema()).unwrap();
    let mut names: Vec<String> = schema["properties"]
        .as_object()
        .unwrap_or_else(|| panic!("{schema}"))
        .keys()
        .cloned()
        .collect();
    names.sort();
    names
}

/// The mirror's schema publishes exactly the SDK type's wire names plus the named REST-only
/// members (the other direction of [`pinned`]: a mirror-only optional member that `pinned` cannot
/// see is caught here). The products `closed_sets_tests.rs` precedent reads the schema the same way.
fn mirrors<D: utoipa::PartialSchema, T: serde::de::DeserializeOwned>(rest_only: &[&str]) {
    let sdk = wire_fields::<T>();
    assert!(
        !sdk.is_empty(),
        "{} is a struct",
        std::any::type_name::<T>()
    );
    let mut expected: Vec<String> = sdk
        .iter()
        .chain(rest_only)
        .map(|name| (*name).to_owned())
        .collect();
    expected.sort();
    assert_eq!(
        schema_properties::<D>(),
        expected,
        "{} mirrors {}",
        std::any::type_name::<D>(),
        std::any::type_name::<T>()
    );
}

#[test]
fn openapi_mirror_schemas_name_exactly_the_sdk_wire_fields() {
    use bss_orders_sdk::reads::{LinePage, OrderPage, OrderSummary, PageInfo};
    // Request bodies: the boundary takes `expected_draft_revision` before the SDK decode (D-147).
    mirrors::<dto::OrdersCreateOrder, CreateOrder>(&[]);
    mirrors::<dto::OrdersAddLine, AddLine>(&["expected_draft_revision"]);
    mirrors::<dto::OrdersHeaderFields, HeaderPatch>(&[]);
    mirrors::<dto::OrdersLineFields, LinePatch>(&[]);
    mirrors::<dto::OrdersSelectedItem, SelectedItem>(&[]);
    // Responses.
    mirrors::<dto::OrdersTransitionResult, TransitionResult>(&[]);
    mirrors::<dto::OrdersOrderView, OrderView>(&[]);
    mirrors::<dto::OrdersOrderHeader, OrderHeader>(&[]);
    mirrors::<dto::OrdersVersionSummary, VersionSummary>(&[]);
    mirrors::<dto::OrdersDraftLine, DraftLine>(&[]);
    mirrors::<dto::OrdersOrderSummary, OrderSummary>(&[]);
    mirrors::<dto::OrdersPageInfo, PageInfo>(&[]);
    mirrors::<dto::OrdersOrderPage, OrderPage>(&[]);
    mirrors::<dto::OrdersLinePage, LinePage>(&[]);
    // The REST-only envelopes have no SDK body: their members are listed outright.
    for (envelope, names) in [
        (
            schema_properties::<dto::OrdersHeaderPatch>(),
            &["expected_draft_revision", "fields"][..],
        ),
        (
            schema_properties::<dto::OrdersLinePatch>(),
            &["expected_draft_revision", "fields"][..],
        ),
        (
            schema_properties::<dto::OrdersRemoveLine>(),
            &["expected_draft_revision"][..],
        ),
    ] {
        assert_eq!(envelope, names);
    }
}

#[test]
fn location_comes_from_the_recorded_order_not_the_stored_body() {
    let id = Uuid::from_u128(7);
    let created = |order_id| Settled {
        // The body names another order: Location never reads it.
        response: StoredResponse::new(
            201,
            json!({"order": {"order_id": Uuid::nil()}}),
            std::collections::BTreeMap::new(),
            None,
        )
        .unwrap(),
        outcome: SettledOutcome::Committed,
        order_id,
        replayed: true,
    };
    let out = with_location(created(Some(id)), "/gw/bss-orders/v1/orders/");
    assert_eq!(
        out.response.headers()["location"],
        format!("/gw/bss-orders/v1/orders/{id}")
    );
    assert!(
        !with_location(created(None), "/orders")
            .response
            .headers()
            .contains_key("location")
    );
    // A stored Location under another case is refused, never duplicated.
    let mut cased = StoredResponse::new(
        201,
        json!({}),
        std::collections::BTreeMap::from([("Location".to_owned(), "/x".to_owned())]),
        None,
    )
    .unwrap();
    assert!(cased.insert_location("/y".to_owned()).is_err());
    assert_eq!(cased.headers().len(), 1);
}

#[test]
fn stored_outcomes_render_verbatim_with_semantic_headers() {
    let response = StoredResponse::new(
        201,
        json!({"order": {"order_id": Uuid::nil()}}),
        std::collections::BTreeMap::from([
            ("etag".to_owned(), "\"1\"".to_owned()),
            ("location".to_owned(), "/x".to_owned()),
        ]),
        None,
    )
    .unwrap();
    let first = Settled {
        response,
        outcome: SettledOutcome::Committed,
        order_id: Some(Uuid::nil()),
        replayed: false,
    };
    let out = render(first.clone());
    assert_eq!(out.status(), StatusCode::CREATED);
    assert_eq!(out.headers()["etag"], "\"1\"");
    assert_eq!(out.headers()["location"], "/x");
    assert!(!out.headers().contains_key("idempotency-replayed"));
    // A replay renders the same stored response plus the replay marker.
    let out = render(Settled {
        replayed: true,
        ..first
    });
    assert_eq!(out.status(), StatusCode::CREATED);
    assert_eq!(out.headers()["idempotency-replayed"], "true");
    let refused = StoredResponse::new(
        409,
        json!({"error_code": "LINE_CAP_EXCEEDED"}),
        std::collections::BTreeMap::new(),
        None,
    )
    .unwrap();
    let out = render(Settled {
        response: refused,
        outcome: SettledOutcome::Refused(Some(Reason::LineCapExceeded)),
        order_id: None,
        replayed: true,
    });
    assert_eq!(out.status(), StatusCode::CONFLICT);
    assert_eq!(out.headers()["idempotency-replayed"], "true");
    assert_eq!(out.headers()["content-type"], "application/problem+json");
    // A missing expected version is 428 with the canonical Orders code.
    let out =
        super::super::error::ApiError::from(OrdersError::Refused(Reason::ExpectedVersionRequired))
            .into_response();
    assert_eq!(out.status().as_u16(), 428);
}
