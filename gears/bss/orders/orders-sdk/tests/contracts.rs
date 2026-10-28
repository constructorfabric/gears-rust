#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code: a failed setup or fixture must stop the test at once; `allow`, not \
              `expect`, because not every test in the file unwraps or expects"
)]
use bss_orders_sdk::{
    OrdersError,
    catalog::{EventKind, OPERATIONS, OrderState, Reason, Trigger},
    errors::RefusalDiagnostics,
    models::{DraftRevision, IdempotencyKey, OrderVersion, SpawnSignalResult},
};
use serde_json::{Value, json};
use toolkit_canonical_errors::{CanonicalError, Problem};

fn catalog() -> Value {
    serde_json::from_str(include_str!("../contracts/catalog.json")).unwrap()
}

#[test]
fn every_operation_and_reason_matches_the_reviewed_catalog() {
    let catalog = catalog();
    assert_eq!(
        OPERATIONS.len(),
        catalog["operations"].as_array().unwrap().len()
    );
    for (op, expected) in OPERATIONS
        .iter()
        .zip(catalog["operations"].as_array().unwrap())
    {
        assert_eq!(op.id, expected["id"]);
        assert_eq!(op.resource, expected["pdp_resource"]);
        assert_eq!(op.action, expected["pdp_action"]);
    }
    assert_eq!(
        Reason::ALL.len(),
        catalog["errors"].as_array().unwrap().len()
    );
    for (reason, expected) in Reason::ALL
        .iter()
        .zip(catalog["errors"].as_array().unwrap())
    {
        let mapping = reason.mapping();
        assert_eq!(mapping.reason, expected["reason"]);
        assert_eq!(mapping.code, expected["error_code"]);
        assert_eq!(mapping.gts_id, expected["gts_registry_key"]);
        // The category is the canonical one the wire `type`/`title` name; the HTTP status and
        // the Problem envelope are the gear REST layer's (D-219, `orders/tests/rest_problems.rs`).
        assert_eq!(
            format!("gts://{}", mapping.category.gts_fragment()),
            expected["type"]
        );
        assert_eq!(mapping.category.title(), expected["title"]);
    }
    for (values, key) in [
        (serde_json::to_value(OrderState::ALL).unwrap(), "states"),
        (serde_json::to_value(Trigger::ALL).unwrap(), "triggers"),
    ] {
        assert_eq!(values, catalog[key]);
    }
    assert_eq!(
        EventKind::ALL.len(),
        catalog["events"].as_array().unwrap().len()
    );
    assert!(serde_json::from_value::<OrderState>(json!("invented")).is_err());
}

/// `token()` is the serde name and `from_token` its inverse, for every variant of the four
/// generated registries; any other spelling is `None`. `TERMINAL` is the catalog's
/// `terminal_states`.
#[test]
fn generated_tokens_are_the_serde_names() {
    fn check<T>(all: &[T], token: fn(T) -> &'static str, from_token: fn(&str) -> Option<T>)
    where
        T: Copy + PartialEq + std::fmt::Debug + serde::Serialize,
    {
        for value in all {
            assert_eq!(serde_json::to_value(value).unwrap(), json!(token(*value)));
            assert_eq!(from_token(token(*value)), Some(*value));
        }
        assert_eq!(from_token("invented"), None);
        assert_eq!(from_token(""), None);
    }
    check(OrderState::ALL, OrderState::token, OrderState::from_token);
    check(Trigger::ALL, Trigger::token, Trigger::from_token);
    check(EventKind::ALL, EventKind::token, EventKind::from_token);
    check(Reason::ALL, Reason::token, Reason::from_token);
    for reason in Reason::ALL {
        assert_eq!(reason.token(), reason.mapping().reason);
    }
    assert_eq!(
        serde_json::to_value(OrderState::TERMINAL).unwrap(),
        catalog()["terminal_states"]
    );
}

#[test]
fn typed_values_cannot_bypass_wire_bounds() {
    for value in [
        json!(0),
        json!(-1),
        json!(2_147_483_648_i64),
        json!(true),
        json!(1.5),
        json!("1"),
    ] {
        assert!(serde_json::from_value::<OrderVersion>(value).is_err());
    }
    assert!(serde_json::from_value::<OrderVersion>(json!(2_147_483_647)).is_ok());
    assert!(serde_json::from_value::<DraftRevision>(json!(0)).is_ok());
    assert!(serde_json::from_value::<DraftRevision>(json!(i64::MAX)).is_ok());
    for value in [json!(-1), json!(true), json!("0")] {
        assert!(serde_json::from_value::<DraftRevision>(value).is_err());
    }
    for value in [
        String::new(),
        "x".repeat(256),
        "\u{e9}".to_owned(),
        "a\0b".to_owned(),
    ] {
        assert!(IdempotencyKey::try_from(value).is_err());
    }
    // Redaction by absence: the Debug rendering never contains the key text.
    let secret = "orders-idem-7f3c9a1e-redaction-probe";
    let key = IdempotencyKey::try_from(secret.to_owned()).unwrap();
    assert!(!format!("{key:?}").contains(secret));
    assert!(!format!("{key:#?}").contains(secret));
    assert_eq!(String::from(key), secret);
    // Surrounding spaces are kept, not trimmed.
    let spaced = IdempotencyKey::try_from(" a ".to_owned()).unwrap();
    assert_eq!(String::from(spaced), " a ");
    assert!(IdempotencyKey::try_from("x".repeat(255)).is_ok());
}

#[test]
fn grant_golden_round_trip_preserves_version_generation_and_microseconds() {
    let fixtures: Value =
        serde_json::from_str(include_str!("fixtures/boundary-fixtures.json")).unwrap();
    let value = &fixtures["golden_envelopes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == "spawn-result")
        .unwrap()["value"];
    let result: SpawnSignalResult = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(i64::from(result.transition.version), 7);
    assert_eq!(result.generation, 3);
    assert_eq!(result.spawn_signal_at.microsecond(), 123_456);
    assert_eq!(serde_json::to_value(result).unwrap(), *value);
}

/// ADR-0005 projection: infallible, the two infrastructure categories Orders answers are named,
/// every other canonical error is preserved whole in `Other`.
#[test]
fn canonical_errors_project_infallibly_with_a_lossless_catch_all() {
    assert!(matches!(
        OrdersError::from(CanonicalError::service_unavailable().create()),
        OrdersError::Unavailable
    ));
    assert!(matches!(
        OrdersError::from(CanonicalError::internal("boom").create()),
        OrdersError::Internal
    ));
    let canonical = CanonicalError::unauthenticated()
        .with_reason("FUTURE_AUTH_REASON")
        .create();
    let OrdersError::Other { canonical: kept } = OrdersError::from(canonical.clone()) else {
        panic!("a category Orders does not model is the catch-all");
    };
    assert_eq!(kept.gts_type(), canonical.gts_type());
    assert_eq!(
        serde_json::to_value(Problem::from(kept)).unwrap(),
        serde_json::to_value(Problem::from(canonical)).unwrap()
    );
    assert_eq!(OrdersError::Unavailable.reason(), None);
}

/// The settled-refusal diagnostics: every permitted member round-trips under its wire name, an
/// absent member stays absent, an unknown member is ignored and a mistyped one fails.
#[test]
fn refusal_diagnostics_pin_their_wire_members() {
    let full = json!({
        "state": "submitted",
        "trigger": "draft-mutate",
        "current_version": 3,
        "draft_revision": 2,
        "request_audit_id": "0b5d2f0e-3c8e-4f7a-a6f4-8f2f6a3c9d10",
        "blocked": [
            {"overlap_scope_key": "k-1", "conflicting_order_id": "6f1c1f5e-1d1a-4c43-9d8e-2b7d2e7f0a11"},
            {"overlap_scope_key": "k-2", "conflicting_order_id": null}
        ]
    });
    let diagnostics: RefusalDiagnostics = serde_json::from_value(full.clone()).unwrap();
    assert_eq!(diagnostics.state, Some(OrderState::Submitted));
    assert_eq!(diagnostics.trigger, Some(Trigger::DraftMutate));
    assert_eq!(diagnostics.current_version.map(i64::from), Some(3));
    assert_eq!(diagnostics.draft_revision.map(i64::from), Some(2));
    assert_eq!(serde_json::to_value(&diagnostics).unwrap(), full);
    assert_eq!(
        serde_json::to_value(RefusalDiagnostics::default()).unwrap(),
        json!({})
    );
    let tolerant: RefusalDiagnostics =
        serde_json::from_value(json!({"current_version": 1, "future": [1]})).unwrap();
    assert_eq!(
        serde_json::to_value(tolerant).unwrap(),
        json!({"current_version": 1})
    );
    assert!(serde_json::from_value::<RefusalDiagnostics>(json!({"current_version": 0})).is_err());
    assert!(serde_json::from_value::<RefusalDiagnostics>(json!({"state": "invented"})).is_err());
}
