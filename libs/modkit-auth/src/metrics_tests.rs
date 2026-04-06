use super::*;

#[test]
fn test_auth_event_metric_names() {
    assert_eq!(AuthEvent::JwtValid.metric_name(), "auth.jwt.valid");
    assert_eq!(AuthEvent::JwtInvalid.metric_name(), "auth.jwt.invalid");
    assert_eq!(
        AuthEvent::JwksRefreshSuccess.metric_name(),
        "auth.jwks.refresh.ok"
    );
    assert_eq!(
        AuthEvent::JwksRefreshFailure.metric_name(),
        "auth.jwks.refresh.fail"
    );
}

#[test]
fn test_metric_labels_builder() {
    let labels = AuthMetricLabels::default()
        .with_provider("keycloak")
        .with_issuer("https://kc.example.com")
        .with_kid("key-123");

    assert_eq!(labels.provider, Some("keycloak".to_owned()));
    assert_eq!(labels.issuer, Some("https://kc.example.com".to_owned()));
    assert_eq!(labels.kid, Some("key-123".to_owned()));
    assert_eq!(labels.error_type, None);
}

#[test]
fn test_noop_metrics() {
    let metrics = NoOpMetrics;
    let labels = AuthMetricLabels::default();

    // Should not panic
    metrics.record_event(AuthEvent::JwtValid, &labels);
    metrics.record_duration(100, &labels);
}

#[test]
fn test_logging_metrics() {
    let metrics = LoggingMetrics;
    let labels = AuthMetricLabels::default()
        .with_provider("test")
        .with_issuer("https://test.example.com");

    // Should not panic
    metrics.record_event(AuthEvent::JwtValid, &labels);
    metrics.record_duration(50, &labels);
}
