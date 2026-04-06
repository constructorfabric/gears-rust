use super::*;

#[test]
fn alias_standard_port_omitted() {
    let ep = Endpoint {
        scheme: Scheme::Https,
        host: "api.openai.com".into(),
        port: 443,
    };
    assert_eq!(ep.alias_contribution(), "api.openai.com");
}

#[test]
fn alias_port_80_omitted() {
    let ep = Endpoint {
        scheme: Scheme::Https,
        host: "example.com".into(),
        port: 80,
    };
    assert_eq!(ep.alias_contribution(), "example.com");
}

#[test]
fn alias_nonstandard_port_included() {
    let ep = Endpoint {
        scheme: Scheme::Https,
        host: "api.openai.com".into(),
        port: 8443,
    };
    assert_eq!(ep.alias_contribution(), "api.openai.com:8443");
}

#[test]
fn default_scheme_is_https() {
    assert_eq!(Scheme::default(), Scheme::Https);
}

#[test]
fn endpoint_round_trip() {
    let ep = Endpoint {
        scheme: Scheme::Wss,
        host: "stream.example.com".into(),
        port: 9090,
    };
    let ep2 = ep.clone();
    assert_eq!(ep, ep2);
}

#[test]
fn route_construction() {
    let route = Route {
        id: Uuid::nil(),
        tenant_id: Uuid::nil(),
        upstream_id: Uuid::nil(),
        match_rules: MatchRules {
            http: Some(HttpMatch {
                methods: vec![HttpMethod::Post],
                path: "/v1/chat/completions".into(),
                query_allowlist: vec![],
                path_suffix_mode: PathSuffixMode::Append,
            }),
            grpc: None,
        },
        plugins: None,
        rate_limit: None,
        cors: None,
        tags: vec![],
        priority: 0,
        enabled: true,
    };
    assert!(route.enabled);
    assert_eq!(route.priority, 0);
    let http = route.match_rules.http.unwrap();
    assert_eq!(http.path_suffix_mode, PathSuffixMode::Append);
    assert!(http.query_allowlist.is_empty());
}

#[test]
fn http_method_equality() {
    assert_eq!(HttpMethod::Post, HttpMethod::Post);
    assert_ne!(HttpMethod::Get, HttpMethod::Post);
}

#[test]
fn default_sharing_mode_is_private() {
    assert_eq!(SharingMode::default(), SharingMode::Private);
}

#[test]
fn default_passthrough_mode_is_none() {
    assert_eq!(PassthroughMode::default(), PassthroughMode::None);
}

#[test]
fn default_rate_limit_algorithm_is_token_bucket() {
    assert_eq!(
        RateLimitAlgorithm::default(),
        RateLimitAlgorithm::TokenBucket
    );
}

#[test]
fn default_window_is_second() {
    assert_eq!(Window::default(), Window::Second);
}

#[test]
fn default_path_suffix_mode_is_append() {
    assert_eq!(PathSuffixMode::default(), PathSuffixMode::Append);
}
