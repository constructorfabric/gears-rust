#![allow(clippy::unwrap_used, clippy::expect_used)]
use bss_orders_lifecycle_sdk::{
    OrdersError,
    catalog::{EventKind, OPERATIONS, OrderState, Reason, Trigger},
    models::{DraftRevision, IdempotencyKey, OrderVersion, SpawnSignalResult},
};
use serde_json::{Value, json};

fn catalog() -> Value {
    serde_json::from_str(include_str!(
        "../../docs/implementation/contracts/catalog.json"
    ))
    .unwrap()
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
        assert_eq!(op.method, expected["method"]);
        assert_eq!(op.path, expected["path"]);
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
        assert_eq!(mapping.http_status, expected["http_status"]);
        let problem = serde_json::to_value(OrdersError::Refused(*reason).into_problem()).unwrap();
        for key in ["type", "title", "error_domain", "error_code"] {
            assert_eq!(problem[key], expected[key]);
        }
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
    let key = IdempotencyKey::try_from(" a ".to_owned()).unwrap();
    assert_eq!(format!("{key:?}"), "IdempotencyKey([redacted])");
    assert_eq!(String::from(key), " a ");
    assert!(IdempotencyKey::try_from("x".repeat(255)).is_ok());
}

#[test]
fn grant_golden_round_trip_preserves_version_generation_and_microseconds() {
    let fixtures: Value = serde_json::from_str(include_str!(
        "../../docs/implementation/contracts/boundary-fixtures.json"
    ))
    .unwrap();
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

#[test]
fn unknown_remote_problem_preserves_its_context() {
    let value = json!({"type":"gts://future.error", "title":"Future error", "status":503,
        "detail":"Retry later", "error_domain":"future.v2", "error_code":"FUTURE",
        "context":{"data":{"opaque":[1,2,3]}}});
    let error = OrdersError::Remote(Box::new(serde_json::from_value(value.clone()).unwrap()));
    assert_eq!(serde_json::to_value(error.into_problem()).unwrap(), value);
}
