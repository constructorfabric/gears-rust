#![allow(clippy::expect_used, clippy::unwrap_used)]
#[test]
fn missing_registry_is_a_retryable_door_error() {
    let hub = toolkit::ClientHub::new();
    let error = bss_pricing::infra::reference_registry::resolve(&hub)
        .err()
        .unwrap();
    let problem = toolkit::api::canonical_prelude::Problem::from(error);
    let json = serde_json::to_value(problem).unwrap();
    assert_eq!(json["status"], 503);
    assert!(json.to_string().contains("REGISTRY_UNAVAILABLE"), "{json}");
}
