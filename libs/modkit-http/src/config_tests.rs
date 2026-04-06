use super::*;

#[test]
fn test_retry_trigger_constants() {
    assert_eq!(RetryTrigger::TOO_MANY_REQUESTS, RetryTrigger::Status(429));
    assert_eq!(RetryTrigger::REQUEST_TIMEOUT, RetryTrigger::Status(408));
    assert_eq!(
        RetryTrigger::INTERNAL_SERVER_ERROR,
        RetryTrigger::Status(500)
    );
    assert_eq!(RetryTrigger::BAD_GATEWAY, RetryTrigger::Status(502));
    assert_eq!(RetryTrigger::SERVICE_UNAVAILABLE, RetryTrigger::Status(503));
    assert_eq!(RetryTrigger::GATEWAY_TIMEOUT, RetryTrigger::Status(504));
}

#[test]
fn test_is_idempotent_method() {
    // Idempotent per RFC 9110
    assert!(is_idempotent_method(&http::Method::GET));
    assert!(is_idempotent_method(&http::Method::HEAD));
    assert!(is_idempotent_method(&http::Method::PUT));
    assert!(is_idempotent_method(&http::Method::DELETE));
    assert!(is_idempotent_method(&http::Method::OPTIONS));
    assert!(is_idempotent_method(&http::Method::TRACE));
    // Non-idempotent
    assert!(!is_idempotent_method(&http::Method::POST));
    assert!(!is_idempotent_method(&http::Method::PATCH));
}

#[test]
fn test_retry_config_defaults() {
    let config = RetryConfig::default();
    assert_eq!(config.max_retries, 3);
    assert_eq!(config.backoff.initial, Duration::from_millis(100));
    assert_eq!(config.backoff.max, Duration::from_secs(10));
    assert!((config.backoff.multiplier - 2.0).abs() < f64::EPSILON);
    assert!(config.backoff.jitter);

    // Check always_retry defaults - only 429 is always retried
    assert!(
        config
            .always_retry
            .contains(&RetryTrigger::TOO_MANY_REQUESTS)
    );
    assert_eq!(config.always_retry.len(), 1);

    // Check idempotent_retry defaults - includes TransportError and Timeout for safety
    assert!(
        config
            .idempotent_retry
            .contains(&RetryTrigger::TransportError)
    );
    assert!(config.idempotent_retry.contains(&RetryTrigger::Timeout));
    assert!(
        config
            .idempotent_retry
            .contains(&RetryTrigger::REQUEST_TIMEOUT)
    );
    assert!(
        config
            .idempotent_retry
            .contains(&RetryTrigger::INTERNAL_SERVER_ERROR)
    );
    assert!(config.idempotent_retry.contains(&RetryTrigger::BAD_GATEWAY));
    assert!(
        config
            .idempotent_retry
            .contains(&RetryTrigger::SERVICE_UNAVAILABLE)
    );
    assert!(
        config
            .idempotent_retry
            .contains(&RetryTrigger::GATEWAY_TIMEOUT)
    );
    assert_eq!(config.idempotent_retry.len(), 7);

    // Default respects Retry-After header
    assert!(!config.ignore_retry_after);

    // Default drain limit
    assert_eq!(
        config.retry_response_drain_limit,
        DEFAULT_RETRY_RESPONSE_DRAIN_LIMIT
    );

    // Default idempotency key header
    assert_eq!(
        config.idempotency_key_header,
        Some(http::header::HeaderName::from_static(
            IDEMPOTENCY_KEY_HEADER_LOWER
        ))
    );
}

#[test]
fn test_retry_config_disabled() {
    let config = RetryConfig::disabled();
    assert_eq!(config.max_retries, 0);
}

#[test]
fn test_retry_config_aggressive() {
    let config = RetryConfig::aggressive();
    assert_eq!(config.max_retries, 5);
    assert_eq!(config.backoff.initial, Duration::from_millis(50));
    assert_eq!(config.backoff.max, Duration::from_secs(30));
    // Aggressive moves all 5xx to always_retry
    assert!(
        config
            .always_retry
            .contains(&RetryTrigger::INTERNAL_SERVER_ERROR)
    );
    assert!(config.idempotent_retry.is_empty());
}

#[test]
fn test_should_retry_always() {
    let config = RetryConfig::default();

    // 429 always retries regardless of method or idempotency key
    assert!(config.should_retry(RetryTrigger::TOO_MANY_REQUESTS, &http::Method::GET, false));
    assert!(config.should_retry(RetryTrigger::TOO_MANY_REQUESTS, &http::Method::POST, false));
    assert!(config.should_retry(RetryTrigger::TOO_MANY_REQUESTS, &http::Method::POST, true));
}

#[test]
fn test_should_retry_idempotent_only() {
    let config = RetryConfig::default();

    // TransportError retries for idempotent methods only (by default)
    assert!(config.should_retry(RetryTrigger::TransportError, &http::Method::GET, false));
    assert!(!config.should_retry(RetryTrigger::TransportError, &http::Method::POST, false));

    // 500 only retries for idempotent methods
    assert!(config.should_retry(
        RetryTrigger::INTERNAL_SERVER_ERROR,
        &http::Method::GET,
        false
    ));
    assert!(!config.should_retry(
        RetryTrigger::INTERNAL_SERVER_ERROR,
        &http::Method::POST,
        false
    ));

    // 503 only retries for idempotent methods
    assert!(config.should_retry(
        RetryTrigger::SERVICE_UNAVAILABLE,
        &http::Method::HEAD,
        false
    ));
    assert!(!config.should_retry(
        RetryTrigger::SERVICE_UNAVAILABLE,
        &http::Method::POST,
        false
    ));

    // Timeout only retries for idempotent methods
    assert!(config.should_retry(RetryTrigger::Timeout, &http::Method::GET, false));
    assert!(!config.should_retry(RetryTrigger::Timeout, &http::Method::POST, false));
}

#[test]
fn test_should_retry_with_idempotency_key() {
    let config = RetryConfig::default();

    // TransportError retries for non-idempotent methods when idempotency key is present
    assert!(config.should_retry(RetryTrigger::TransportError, &http::Method::POST, true));
    assert!(config.should_retry(RetryTrigger::TransportError, &http::Method::PUT, true));
    assert!(config.should_retry(RetryTrigger::TransportError, &http::Method::DELETE, true));
    assert!(config.should_retry(RetryTrigger::TransportError, &http::Method::PATCH, true));

    // Timeout retries for non-idempotent methods when idempotency key is present
    assert!(config.should_retry(RetryTrigger::Timeout, &http::Method::POST, true));

    // 500 retries for non-idempotent methods when idempotency key is present
    assert!(config.should_retry(
        RetryTrigger::INTERNAL_SERVER_ERROR,
        &http::Method::POST,
        true
    ));
}

#[test]
fn test_should_retry_not_configured() {
    let config = RetryConfig::default();

    // 400 Bad Request is not in any retry set
    assert!(!config.should_retry(RetryTrigger::Status(400), &http::Method::GET, false));
    assert!(!config.should_retry(RetryTrigger::Status(400), &http::Method::POST, false));
    assert!(!config.should_retry(RetryTrigger::Status(400), &http::Method::POST, true)); // Even with idempotency key

    // 404 Not Found is not in any retry set
    assert!(!config.should_retry(RetryTrigger::Status(404), &http::Method::GET, false));
}

#[test]
fn test_rate_limit_config_defaults() {
    let config = RateLimitConfig::default();
    assert_eq!(config.max_concurrent_requests, 100);
}

#[test]
fn test_rate_limit_config_unlimited() {
    let config = RateLimitConfig::unlimited();
    assert_eq!(config.max_concurrent_requests, usize::MAX);
}

#[test]
fn test_rate_limit_config_conservative() {
    let config = RateLimitConfig::conservative();
    assert_eq!(config.max_concurrent_requests, 10);
}

#[test]
fn test_http_client_config_defaults() {
    let config = HttpClientConfig::default();
    assert_eq!(config.request_timeout, Duration::from_secs(30));
    assert_eq!(config.max_body_size, 10 * 1024 * 1024);
    assert_eq!(config.user_agent, DEFAULT_USER_AGENT);
    assert!(config.retry.is_some());
    assert!(config.rate_limit.is_some());
    assert_eq!(config.transport, TransportSecurity::AllowInsecureHttp);
    assert!(!config.otel);
    assert_eq!(config.buffer_capacity, 1024);
}

#[test]
fn test_http_client_config_minimal() {
    let config = HttpClientConfig::minimal();
    assert_eq!(config.request_timeout, Duration::from_secs(10));
    assert_eq!(config.max_body_size, 1024 * 1024);
    assert!(config.retry.is_none());
    assert!(config.rate_limit.is_none());
}

#[test]
fn test_http_client_config_infra_default() {
    let config = HttpClientConfig::infra_default();
    assert_eq!(config.request_timeout, Duration::from_secs(60));
    assert_eq!(config.max_body_size, 50 * 1024 * 1024);
    assert!(config.retry.is_some());
    assert_eq!(config.retry.unwrap().max_retries, 5);
}

#[test]
fn test_http_client_config_token_endpoint() {
    let config = HttpClientConfig::token_endpoint();
    assert_eq!(config.request_timeout, Duration::from_secs(30));

    let retry = config.retry.unwrap();
    // Token endpoint: no idempotent-only retries (conservative for auth)
    assert!(retry.idempotent_retry.is_empty());
    // But still retry transport errors and 429
    assert!(retry.always_retry.contains(&RetryTrigger::TransportError));
    assert!(
        retry
            .always_retry
            .contains(&RetryTrigger::TOO_MANY_REQUESTS)
    );

    let rate_limit = config.rate_limit.unwrap();
    assert_eq!(rate_limit.max_concurrent_requests, 10); // Conservative
}

#[test]
fn test_http_client_config_for_testing() {
    let config = HttpClientConfig::for_testing();
    assert_eq!(config.transport, TransportSecurity::AllowInsecureHttp);
    assert!(config.retry.is_none());
}

#[test]
fn test_http_client_config_sse() {
    let config = HttpClientConfig::sse();
    assert_eq!(config.request_timeout, Duration::from_secs(86_400));
    assert!(config.total_timeout.is_none());
    assert!(config.retry.is_none());
    assert!(config.rate_limit.is_none());
    assert!(!config.otel);
    assert_eq!(config.buffer_capacity, 64);
    assert!(config.pool_idle_timeout.is_none());
    assert_eq!(config.pool_max_idle_per_host, 1);
}
