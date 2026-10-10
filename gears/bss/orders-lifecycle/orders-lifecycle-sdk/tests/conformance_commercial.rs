//! S1-06 native receipt vectors and maximum-basket fixture construction.
use bss_orders_lifecycle_sdk::{
    commercial::{AcceptedVersionRef, AssessmentRef, MAX_VERSION_BYTES, OrderPin},
    models::OrderVersion,
};
use bss_pricing_sdk::{acceptance::Term, digest};
use serde_json::Value;
use std::collections::BTreeSet;
use uuid::Uuid;
#[allow(clippy::expect_used)] // A corrupt frozen test fixture must stop the test immediately.
fn fixtures() -> Value {
    serde_json::from_str(include_str!("fixtures/commercial-conformance.json"))
        .expect("frozen commercial fixture")
}

#[test]
fn independent_finite_rolling_and_payer_profiles_keep_exact_native_evidence() {
    let all = fixtures();
    for v in all["pins"].as_array().unwrap() {
        let pin = OrderPin::decode(&serde_json::to_vec(&v["pin"]).unwrap())
            .unwrap_or_else(|e| panic!("{}: {e:?}", v["id"]));
        assert_eq!(
            serde_json::from_slice::<Value>(&pin.encode().unwrap()).unwrap(),
            v["pin"]
        );
        let receipt = pin.receipt();
        pin.verify_selection(
            &receipt.query,
            receipt.acceptance_id,
            receipt.request_digest,
            receipt.terms_digest,
            &receipt.bindings,
        )
        .unwrap();
        assert_eq!(receipt.query.order_version, 7);
        assert_eq!(
            receipt.bindings.len(),
            if v["id"] == "single-recurring-basket-seed" {
                1
            } else {
                3
            }
        );
        if receipt.bindings.len() > 1 {
            assert_eq!(receipt.query.selections[0].dimension_value, None);
            assert_eq!(
                receipt.query.selections[1].dimension_value.as_deref(),
                Some("default")
            );
        }
        assert_eq!(
            matches!(receipt.query.term, Term::Rolling),
            v["id"] == "rolling-mixed-charge-kinds"
        );
        assert_eq!(
            receipt.query.tenant_axes.payer_tenant_id
                == receipt.query.tenant_axes.resource_tenant_id,
            v["id"] != "partner-third-party-finite-year"
        );
    }
}

#[test]
fn two_hundred_line_corpus_has_unique_receipts_and_round_trips() {
    let all = fixtures();
    let seed = &all["pins"][3]["pin"];
    let original = OrderPin::decode(&serde_json::to_vec(seed).unwrap()).unwrap();
    let mut lines = BTreeSet::new();
    let mut acceptances = BTreeSet::new();
    let mut total_bytes = 0;
    for index in 0..200u128 {
        let mut receipt = original.receipt().clone();
        receipt.query.line_id = Uuid::from_u128(10_000 + index);
        receipt.acceptance_id = Uuid::from_u128(20_000 + index);
        receipt.request_digest = digest::request_digest(&receipt.query);
        receipt.terms_digest = digest::terms_digest(&receipt.query, &receipt.bindings);
        assert!(lines.insert(receipt.query.line_id));
        assert!(acceptances.insert(receipt.acceptance_id));
        let pin = OrderPin::new(
            AssessmentRef {
                assessment_id: Uuid::from_u128(30_000 + index),
                assessed_at: receipt.accepted_at,
                resolve_date: receipt.accepted_at.date(),
            },
            AcceptedVersionRef {
                order_id: receipt.query.order_id,
                order_version: OrderVersion::try_from(7).unwrap(),
                line_id: receipt.query.line_id,
            },
            receipt,
        )
        .unwrap();
        let bytes = pin.encode().unwrap();
        total_bytes += bytes.len();
        assert_eq!(OrderPin::decode(&bytes).unwrap(), pin);
    }
    assert_eq!(lines.len(), 200);
    assert_eq!(acceptances.len(), 200);
    // Receipt-only size; the future whole-version serializer must also budget all other fields.
    assert!(
        total_bytes < MAX_VERSION_BYTES,
        "receipt fixture footprint: {total_bytes}"
    );
}
