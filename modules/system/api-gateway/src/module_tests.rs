use super::*;

#[test]
fn test_openapi_generation() {
    let mut config = ApiGatewayConfig::default();
    config.openapi.title = "Test API".to_owned();
    config.openapi.version = "1.0.0".to_owned();
    config.openapi.description = Some("Test Description".to_owned());
    let api = ApiGateway::new(config);

    // Test that we can build OpenAPI without any operations
    let doc = api.build_openapi().unwrap();
    let json = serde_json::to_value(&doc).unwrap();

    // Verify it's valid OpenAPI document structure
    assert!(json.get("openapi").is_some());
    assert!(json.get("info").is_some());
    assert!(json.get("paths").is_some());

    // Verify info section
    let info = json.get("info").unwrap();
    assert_eq!(info.get("title").unwrap(), "Test API");
    assert_eq!(info.get("version").unwrap(), "1.0.0");
    assert_eq!(info.get("description").unwrap(), "Test Description");
}
