use super::*;

#[test]
fn test_default_config() {
    let cfg = GrpcClientConfig::default();
    assert_eq!(cfg.connect_timeout, Duration::from_secs(10));
    assert_eq!(cfg.rpc_timeout, Duration::from_secs(30));
    assert_eq!(cfg.max_retries, 3);
    assert!(cfg.enable_metrics);
    assert!(cfg.enable_tracing);
}

#[test]
fn test_config_builder() {
    let cfg = GrpcClientConfig::new("test_service")
        .with_connect_timeout(Duration::from_secs(5))
        .with_rpc_timeout(Duration::from_secs(15))
        .with_max_retries(5)
        .without_metrics()
        .without_tracing();

    assert_eq!(cfg.service_name, "test_service");
    assert_eq!(cfg.connect_timeout, Duration::from_secs(5));
    assert_eq!(cfg.rpc_timeout, Duration::from_secs(15));
    assert_eq!(cfg.max_retries, 5);
    assert!(!cfg.enable_metrics);
    assert!(!cfg.enable_tracing);
}

#[test]
fn test_build_endpoint_succeeds() {
    let cfg = GrpcClientConfig::default();
    let result = build_endpoint("http://localhost:50051".to_owned(), &cfg);
    assert!(
        result.is_ok(),
        "build_endpoint should succeed with valid URI"
    );
}

#[test]
fn test_build_endpoint_empty_uri() {
    let cfg = GrpcClientConfig::default();
    let result = build_endpoint(String::new(), &cfg);
    assert!(result.is_err(), "build_endpoint should fail with empty URI");
}
