use super::*;

#[test]
fn test_service_endpoint_creation() {
    let http_ep = ServiceEndpoint::http("localhost", 8080);
    assert_eq!(http_ep.uri, concat!("http", "://localhost:8080"));

    let https_endpoint = ServiceEndpoint::https("localhost", 8443);
    assert_eq!(https_endpoint.uri, "https://localhost:8443");

    let uds_ep = ServiceEndpoint::uds("/tmp/socket.sock");
    assert!(uds_ep.uri.starts_with("unix://"));
    assert!(uds_ep.uri.contains("socket.sock"));

    let custom_ep = ServiceEndpoint::new(concat!("http", "://example.com"));
    assert_eq!(custom_ep.uri, concat!("http", "://example.com"));
}

#[test]
fn test_register_instance_info() {
    let info = RegisterInstanceInfo {
        module: "test_module".to_owned(),
        instance_id: "instance1".to_owned(),
        grpc_services: vec![(
            "test.Service".to_owned(),
            ServiceEndpoint::http("127.0.0.1", 8001),
        )],
        version: Some("1.0.0".to_owned()),
    };

    assert_eq!(info.module, "test_module");
    assert_eq!(info.instance_id, "instance1");
    assert_eq!(info.grpc_services.len(), 1);
}
