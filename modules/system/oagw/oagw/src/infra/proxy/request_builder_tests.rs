use super::*;

fn endpoint(host: &str, port: u16) -> Endpoint {
    Endpoint {
        scheme: Scheme::Https,
        host: host.into(),
        port,
    }
}

#[test]
fn standard_url() {
    let url = build_upstream_url(
        &endpoint("api.openai.com", 443),
        "/v1/chat",
        "/completions",
        &[],
    )
    .unwrap();
    assert_eq!(url, "https://api.openai.com/v1/chat/completions");
}

#[test]
fn with_query_params() {
    let url = build_upstream_url(
        &endpoint("api.openai.com", 443),
        "/v1/chat",
        "/models/gpt-4",
        &[("version".into(), "2".into())],
    )
    .unwrap();
    assert_eq!(url, "https://api.openai.com/v1/chat/models/gpt-4?version=2");
}

#[test]
fn nonstandard_port() {
    let url = build_upstream_url(&endpoint("localhost", 8080), "/api", "", &[]).unwrap();
    assert_eq!(url, "https://localhost:8080/api");
}

#[test]
fn empty_suffix() {
    let url = build_upstream_url(&endpoint("api.openai.com", 443), "/v1/models", "", &[]).unwrap();
    assert_eq!(url, "https://api.openai.com/v1/models");
}

#[test]
fn avoids_double_slash() {
    let url = build_upstream_url(&endpoint("api.openai.com", 443), "/v1/", "/chat", &[]).unwrap();
    assert_eq!(url, "https://api.openai.com/v1/chat");
}

#[test]
fn multiple_query_params() {
    let url = build_upstream_url(
        &endpoint("example.com", 443),
        "/api",
        "/data",
        &[("key".into(), "val".into()), ("foo".into(), "bar".into())],
    )
    .unwrap();
    assert_eq!(url, "https://example.com/api/data?key=val&foo=bar");
}

#[test]
fn http_scheme() {
    let ep = Endpoint {
        scheme: Scheme::Http,
        host: "127.0.0.1".into(),
        port: 3000,
    };
    let url = build_upstream_url(&ep, "/v1/test", "", &[]).unwrap();
    assert_eq!(url, "http://127.0.0.1:3000/v1/test");
}

#[test]
fn http_default_port() {
    let ep = Endpoint {
        scheme: Scheme::Http,
        host: "example.com".into(),
        port: 80,
    };
    let url = build_upstream_url(&ep, "/api", "", &[]).unwrap();
    assert_eq!(url, "http://example.com/api");
}

#[test]
fn query_value_with_ampersand_is_encoded() {
    let url = build_upstream_url(
        &endpoint("api.openai.com", 443),
        "/v1/search",
        "",
        &[("q".into(), "a&b".into())],
    )
    .unwrap();
    assert_eq!(url, "https://api.openai.com/v1/search?q=a%26b");
}

#[test]
fn grpc_scheme_returns_error() {
    let ep = Endpoint {
        scheme: Scheme::Grpc,
        host: "grpc.example.com".into(),
        port: 443,
    };
    let err = build_upstream_url(&ep, "/service", "", &[]).unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }));
}
