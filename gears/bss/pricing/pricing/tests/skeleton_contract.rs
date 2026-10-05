//! Demolition contract, written before the legacy implementation is removed.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use bss_pricing::module::BssPricingGear;
use toolkit::contracts::DatabaseCapability;

#[test]
fn runtime_chain_contains_coord_pricing_and_toolkit_delivery() {
    let chain = BssPricingGear::default().migrations();
    assert_eq!(
        chain.len(),
        27,
        "the schema guard, coordination, twenty-three pricing migrations (000013, D-427; 000014, \
         D-438; 000015, the book's description; 000016, the unit's note, D-445; 000017, the \
         scheduled revision, D-446; 000018, entry policies, D-502; 000019, commercial receipts, D-505; \
         000020, the plan summary, D-484; 000021, the policy references the SKU revision, D-514; \
         000022, cancel and end, D-520; 000023, the book's archive, D-522) \
         and two toolkit migrations"
    );
    assert_eq!(
        chain[0].name(),
        "m0000_pricing_refuse_a_legacy_or_stale_schema"
    );
    assert_eq!(chain[1].name(), "m0001_create_coord_leases");
}

#[test]
fn skeleton_config_tolerates_old_deployment_keys() {
    let config = serde_json::json!({"legacy_removed_setting": {"enabled": true}});
    serde_json::from_value::<bss_pricing::config::BssPricingConfig>(config)
        .expect("the skeleton tolerates retired deployment fields");
}

pub mod rest_support;

#[tokio::test]
async fn pricing_alone_initializes_serves_authoring_routes_and_stops() {
    use axum::{
        Router,
        body::Body,
        http::{Request, StatusCode},
        routing::get,
    };
    use std::sync::{Arc, atomic::Ordering};
    use toolkit::lifecycle::Runnable;
    use tower::ServiceExt as _;

    let harness = rest_support::Harness::new().await.unwrap();
    assert_eq!(
        harness.registry.calls.load(Ordering::SeqCst),
        1,
        "real init must register schemas"
    );
    let (empty, openapi) = harness.router(Router::new()).unwrap();
    assert!(empty.has_routes());
    // 60: run 9.7's batch checks read, run 9.8b's plans counts, cancel and end, and the book's
    // archive and unarchive (D-522).
    assert_eq!(openapi.operation_specs.len(), 60);
    let (router, _) = harness
        .router(Router::new().route("/host", get(|| async { "host" })))
        .unwrap();
    for (path, status) in [
        ("/bss-pricing/v1/anything", StatusCode::NOT_FOUND),
        // Promotions are deferred by the owner (D-409): nothing is mounted there.
        ("/bss-pricing/v1/promotions", StatusCode::NOT_FOUND),
        ("/host", StatusCode::OK),
    ] {
        let response = router
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), status, "{path}");
    }
    let cancel = tokio_util::sync::CancellationToken::new();
    let mut lifecycle = Box::pin(Arc::new(harness.gear).run(cancel.clone()));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(10), &mut lifecycle)
            .await
            .is_err(),
        "serve must remain live until shutdown"
    );
    cancel.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(1), lifecycle)
        .await
        .unwrap()
        .unwrap();
}

// Surface S-5: each plan request body documents its own door in the served spec; the clone body
// is not described as the rename under If-Match.
#[tokio::test]
async fn the_served_spec_documents_each_plan_body_on_its_own_schema() {
    use toolkit::api::OpenApiInfo;
    let harness = rest_support::Harness::new().await.unwrap();
    let (_, openapi) = harness.router(axum::Router::new()).unwrap();
    let api =
        serde_json::to_value(openapi.build_openapi(&OpenApiInfo::default()).unwrap()).unwrap();
    let described = |schema: &str| {
        api["components"]["schemas"][schema]["description"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    };
    let patch = described("PricingPlanPatch");
    assert!(
        patch.contains("PATCH /plans/{id}") && patch.contains("If-Match"),
        "{patch:?}"
    );
    let clone = described("PricingPlanClone");
    assert!(clone.contains("POST /plans/{id}/clone"), "{clone:?}");
    assert!(!clone.contains("PATCH"), "{clone:?}");
}

// The served door descriptions say what the doors do: a rejected prices unit leaves its prices
// rejected (only a plan revision returns to draft), and a paired half submitted alone is 400
// PAIR_SPLIT, a pair going through publish-changes (D-405).
#[tokio::test]
async fn the_served_reject_and_submit_descriptions_say_what_they_do() {
    use toolkit::api::OpenApiInfo;
    let harness = rest_support::Harness::new().await.unwrap();
    let (_, openapi) = harness.router(axum::Router::new()).unwrap();
    let api =
        serde_json::to_value(openapi.build_openapi(&OpenApiInfo::default()).unwrap()).unwrap();
    let described = |path: &str| {
        api["paths"][path]["post"]["description"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    };
    let reject = described("/bss-pricing/v1/approval-units/{id}/reject");
    assert!(
        reject.contains("plan revision returns to draft") && reject.contains("stay rejected"),
        "{reject:?}"
    );
    assert!(
        !reject.contains("returns its content to draft"),
        "{reject:?}"
    );
    let submit = described("/bss-pricing/v1/prices/{id}/submit");
    assert!(
        submit.contains("PAIR_SPLIT") && submit.contains("publish-changes"),
        "{submit:?}"
    );
    assert!(!submit.contains("with its pair partner"), "{submit:?}");
}
