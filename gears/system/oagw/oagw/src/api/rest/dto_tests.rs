use super::CircuitBreakerConfig;
use crate::domain::model::CircuitBreakerConfig as Domain;

/// `{}` takes the SDK's (ADR-0005) defaults.
#[test]
fn circuit_breaker_defaults_are_the_sdks() {
    let dto: CircuitBreakerConfig = serde_json::from_str("{}").unwrap();
    let sdk = oagw_sdk::CircuitBreakerConfig::default();
    assert_eq!(
        (
            dto.enabled,
            dto.failure_threshold,
            dto.timeout_seconds,
            dto.success_threshold
        ),
        (
            sdk.enabled,
            sdk.failure_threshold,
            sdk.timeout_seconds,
            sdk.success_threshold
        )
    );
}

/// The schema documents the bounds and defaults validation and serde apply.
#[test]
fn circuit_breaker_schema_matches_validation() {
    let schema =
        serde_json::to_value(<CircuitBreakerConfig as utoipa::PartialSchema>::schema()).unwrap();
    let sdk = oagw_sdk::CircuitBreakerConfig::default();
    for (field, max, default) in [
        (
            "failure_threshold",
            u64::from(Domain::MAX_FAILURE_THRESHOLD),
            sdk.failure_threshold,
        ),
        (
            "timeout_seconds",
            crate::config::MAX_PROXY_TIMEOUT_SECS,
            sdk.timeout_seconds,
        ),
        (
            "success_threshold",
            u64::from(Domain::MAX_SUCCESS_THRESHOLD),
            sdk.success_threshold,
        ),
    ] {
        let p = &schema["properties"][field];
        assert_eq!(p["minimum"], 1, "{field}");
        assert_eq!(p["maximum"], max, "{field}");
        assert_eq!(p["default"], default, "{field}");
    }
    assert_eq!(schema["additionalProperties"], false);
}
