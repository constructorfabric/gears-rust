#![allow(clippy::unwrap_used)]
use bss_orders_lifecycle::api::rest;
use bss_orders_lifecycle_sdk::{OrdersError, catalog::Reason};
use serde_json::Value;

#[test]
fn independent_etag_and_problem_goldens_match_real_rust_adapters() {
    let fixtures: Value = serde_json::from_str(include_str!(
        "../../docs/implementation/contracts/boundary-fixtures.json"
    ))
    .unwrap();
    for case in fixtures["cases"].as_array().unwrap() {
        if case["kind"] == "etag" {
            let result = rest::expected_version(case["input"].as_str());
            if let Some(n) = case["expected"].as_i64() {
                assert_eq!(i64::from(result.unwrap()), n);
            } else {
                assert_eq!(rest::problem(result.unwrap_err()).status, Some(428));
            }
        }
        if case["kind"] == "problem" {
            let reason: Reason = serde_json::from_value(case["reason"].clone()).unwrap();
            let p = serde_json::to_value(rest::problem(OrdersError::Refused(reason))).unwrap();
            for k in ["type", "title", "error_domain", "error_code"] {
                assert_eq!(p[k], case["expected"][k]);
            }
            assert_eq!(p["status"], case["expected"]["http_status"]);
        }
    }
    for golden in fixtures["golden_envelopes"].as_array().unwrap() {
        if golden["kind"] == "problem" {
            let reason = Reason::ALL
                .iter()
                .find(|r| r.mapping().code == golden["value"]["error_code"])
                .unwrap();
            assert_eq!(
                serde_json::to_value(rest::problem(OrdersError::Refused(*reason))).unwrap(),
                golden["value"]
            );
        }
    }
}

#[test]
fn submit_requires_independent_revision_and_rejects_context_fields() {
    for body in [
        r"{}",
        r#"{"expected_draft_revision":-1}"#,
        r#"{"expected_draft_revision":true}"#,
        r#"{"expected_draft_revision":0,"actor":"forged"}"#,
        r#"{"expected_draft_revision":0,"expected_version":1}"#,
    ] {
        assert!(serde_json::from_str::<rest::SubmitBody>(body).is_err());
    }
    assert!(serde_json::from_str::<rest::SubmitBody>(r#"{"expected_draft_revision":0}"#).is_ok());
    assert_eq!(
        rest::problem(rest::expected_version(None).unwrap_err()).status,
        Some(428)
    );
    assert!(rest::idempotency_key(None).is_err());
    assert!(rest::idempotency_key(Some("k")).is_ok());
}
