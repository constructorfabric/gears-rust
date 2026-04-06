use super::*;

#[test]
fn hop_by_hop_stripped() {
    let mut headers = HeaderMap::new();
    headers.insert("connection", "keep-alive".parse().unwrap());
    headers.insert("transfer-encoding", "chunked".parse().unwrap());
    headers.insert("x-custom", "keep-me".parse().unwrap());

    strip_hop_by_hop(&mut headers);

    assert!(headers.get("connection").is_none());
    assert!(headers.get("transfer-encoding").is_none());
    assert_eq!(headers.get("x-custom").unwrap(), "keep-me");
}

#[test]
fn hop_by_hop_strips_connection_listed_headers() {
    let mut headers = HeaderMap::new();
    headers.insert("connection", "keep-alive, X-Custom-Hop".parse().unwrap());
    headers.insert("x-custom-hop", "secret".parse().unwrap());
    headers.insert("x-safe", "keep-me".parse().unwrap());

    strip_hop_by_hop(&mut headers);

    assert!(headers.get("connection").is_none());
    assert!(headers.get("x-custom-hop").is_none());
    assert_eq!(headers.get("x-safe").unwrap(), "keep-me");
}

#[test]
fn hop_by_hop_connection_whitespace_handling() {
    let mut headers = HeaderMap::new();
    headers.insert("connection", "keep-alive , X-Foo , X-Bar".parse().unwrap());
    headers.insert("x-foo", "val1".parse().unwrap());
    headers.insert("x-bar", "val2".parse().unwrap());
    headers.insert("x-safe", "keep".parse().unwrap());

    strip_hop_by_hop(&mut headers);

    assert!(headers.get("x-foo").is_none());
    assert!(headers.get("x-bar").is_none());
    assert_eq!(headers.get("x-safe").unwrap(), "keep");
}

#[test]
fn hop_by_hop_no_connection_header() {
    let mut headers = HeaderMap::new();
    headers.insert("transfer-encoding", "chunked".parse().unwrap());
    headers.insert("x-custom", "keep-me".parse().unwrap());

    strip_hop_by_hop(&mut headers);

    assert!(headers.get("transfer-encoding").is_none());
    assert_eq!(headers.get("x-custom").unwrap(), "keep-me");
}

#[test]
fn hop_by_hop_connection_empty_and_invalid_tokens() {
    let mut headers = HeaderMap::new();
    headers.insert("connection", ",,,".parse().unwrap());
    headers.insert("x-custom", "keep-me".parse().unwrap());

    strip_hop_by_hop(&mut headers);

    assert!(headers.get("connection").is_none());
    assert_eq!(headers.get("x-custom").unwrap(), "keep-me");
}

#[test]
fn host_replaced() {
    let mut headers = HeaderMap::new();
    headers.insert(http::header::HOST, "original.com".parse().unwrap());

    set_host_header(&mut headers, "api.openai.com", 443);

    assert_eq!(headers.get(http::header::HOST).unwrap(), "api.openai.com");
}

#[test]
fn host_nonstandard_port() {
    let mut headers = HeaderMap::new();
    set_host_header(&mut headers, "api.openai.com", 8443);

    assert_eq!(
        headers.get(http::header::HOST).unwrap(),
        "api.openai.com:8443"
    );
}

#[test]
fn internal_headers_stripped() {
    let mut headers = HeaderMap::new();
    headers.insert("x-oagw-target-host", "evil.com".parse().unwrap());
    headers.insert("x-oagw-trace-id", "abc".parse().unwrap());
    headers.insert("x-custom", "keep".parse().unwrap());

    strip_internal_headers(&mut headers);

    assert!(headers.get("x-oagw-target-host").is_none());
    assert!(headers.get("x-oagw-trace-id").is_none());
    assert_eq!(headers.get("x-custom").unwrap(), "keep");
}

/// Client-supplied internal headers (including x-oagw-internal-resolved-addr)
/// must be stripped before service.rs injects its own values.
/// This prevents a malicious client from influencing upstream routing.
#[test]
fn strip_removes_spoofed_internal_context_headers() {
    let mut headers = HeaderMap::new();
    // Simulate a malicious client injecting all internal context headers.
    headers.insert("x-oagw-internal-endpoint-host", "evil.com".parse().unwrap());
    headers.insert("x-oagw-internal-endpoint-port", "9999".parse().unwrap());
    headers.insert("x-oagw-internal-endpoint-scheme", "http".parse().unwrap());
    headers.insert(
        "x-oagw-internal-resolved-addr",
        "1.2.3.4:443".parse().unwrap(),
    );
    headers.insert("x-oagw-internal-instance-uri", "/pwned".parse().unwrap());
    headers.insert(
        "x-oagw-internal-upstream-id",
        "00000000-0000-0000-0000-000000000000".parse().unwrap(),
    );
    // Legitimate header that should survive.
    headers.insert("authorization", "Bearer token".parse().unwrap());

    strip_internal_headers(&mut headers);

    assert!(headers.get("x-oagw-internal-endpoint-host").is_none());
    assert!(headers.get("x-oagw-internal-endpoint-port").is_none());
    assert!(headers.get("x-oagw-internal-endpoint-scheme").is_none());
    assert!(headers.get("x-oagw-internal-resolved-addr").is_none());
    assert!(headers.get("x-oagw-internal-instance-uri").is_none());
    assert!(headers.get("x-oagw-internal-upstream-id").is_none());
    assert_eq!(headers.get("authorization").unwrap(), "Bearer token");
}

#[test]
fn set_overwrites_existing() {
    let mut headers = HeaderMap::new();
    headers.insert("x-api-version", "v1".parse().unwrap());

    let rules = RequestHeaderRules {
        set: {
            let mut m = HashMap::new();
            m.insert("x-api-version".into(), "v2".into());
            m
        },
        add: HashMap::new(),
        remove: vec![],
        passthrough: PassthroughMode::None,
        passthrough_allowlist: vec![],
    };

    apply_request_header_rules(&mut headers, &rules);
    assert_eq!(headers.get("x-api-version").unwrap(), "v2");
}

#[test]
fn add_appends() {
    let mut headers = HeaderMap::new();
    headers.insert("x-tag", "a".parse().unwrap());

    let rules = RequestHeaderRules {
        set: HashMap::new(),
        add: {
            let mut m = HashMap::new();
            m.insert("x-tag".into(), "b".into());
            m
        },
        remove: vec![],
        passthrough: PassthroughMode::None,
        passthrough_allowlist: vec![],
    };

    apply_request_header_rules(&mut headers, &rules);
    let values: Vec<&str> = headers
        .get_all("x-tag")
        .iter()
        .map(|v| v.to_str().unwrap())
        .collect();
    assert!(values.contains(&"a"));
    assert!(values.contains(&"b"));
}

#[test]
fn remove_deletes() {
    let mut headers = HeaderMap::new();
    headers.insert("x-remove-me", "gone".parse().unwrap());
    headers.insert("x-keep-me", "stay".parse().unwrap());

    let rules = RequestHeaderRules {
        set: HashMap::new(),
        add: HashMap::new(),
        remove: vec!["x-remove-me".into()],
        passthrough: PassthroughMode::None,
        passthrough_allowlist: vec![],
    };

    apply_request_header_rules(&mut headers, &rules);
    assert!(headers.get("x-remove-me").is_none());
    assert_eq!(headers.get("x-keep-me").unwrap(), "stay");
}

#[test]
fn passthrough_none_starts_empty_but_keeps_content_type() {
    let mut inbound = HeaderMap::new();
    inbound.insert("x-custom", "val".parse().unwrap());
    inbound.insert(
        http::header::CONTENT_TYPE,
        "application/json".parse().unwrap(),
    );

    let out = apply_passthrough(&inbound, &PassthroughMode::None, &[]);

    assert!(out.get("x-custom").is_none());
    assert_eq!(
        out.get(http::header::CONTENT_TYPE).unwrap(),
        "application/json"
    );
}

#[test]
fn passthrough_all_copies_everything() {
    let mut inbound = HeaderMap::new();
    inbound.insert("x-custom", "val".parse().unwrap());
    inbound.insert("x-other", "val2".parse().unwrap());

    let out = apply_passthrough(&inbound, &PassthroughMode::All, &[]);
    assert_eq!(out.len(), 2);
}

#[test]
fn passthrough_allowlist_filters() {
    let mut inbound = HeaderMap::new();
    inbound.insert("x-allowed", "yes".parse().unwrap());
    inbound.insert("x-blocked", "no".parse().unwrap());

    let out = apply_passthrough(&inbound, &PassthroughMode::Allowlist, &["x-allowed".into()]);

    assert_eq!(out.get("x-allowed").unwrap(), "yes");
    assert!(out.get("x-blocked").is_none());
}

#[test]
fn passthrough_all_strips_authorization() {
    let mut inbound = HeaderMap::new();
    inbound.insert(
        http::header::AUTHORIZATION,
        "Bearer secret".parse().unwrap(),
    );
    inbound.insert("cookie", "session=abc".parse().unwrap());
    inbound.insert("x-custom", "keep".parse().unwrap());

    let out = apply_passthrough(&inbound, &PassthroughMode::All, &[]);

    assert!(out.get(http::header::AUTHORIZATION).is_none());
    assert!(out.get("cookie").is_none());
    assert_eq!(out.get("x-custom").unwrap(), "keep");
}

#[test]
fn extract_error_source_gateway() {
    let mut headers = HeaderMap::new();
    headers.insert("x-oagw-error-source", "gateway".parse().unwrap());
    assert_eq!(extract_error_source(&headers), ErrorSource::Gateway);
}

#[test]
fn extract_error_source_absent_defaults_to_upstream() {
    let headers = HeaderMap::new();
    assert_eq!(extract_error_source(&headers), ErrorSource::Upstream);
}

#[test]
fn extract_error_source_unrecognised_defaults_to_upstream() {
    let mut headers = HeaderMap::new();
    headers.insert("x-oagw-error-source", "unknown".parse().unwrap());
    assert_eq!(extract_error_source(&headers), ErrorSource::Upstream);
}

#[test]
fn extract_error_source_upstream_explicit() {
    let mut headers = HeaderMap::new();
    headers.insert("x-oagw-error-source", "upstream".parse().unwrap());
    assert_eq!(extract_error_source(&headers), ErrorSource::Upstream);
}

#[test]
fn sanitize_response_strips_transfer_encoding() {
    let mut headers = HeaderMap::new();
    headers.insert("transfer-encoding", "chunked".parse().unwrap());
    headers.insert("content-type", "application/json".parse().unwrap());

    sanitize_response_headers(&mut headers);

    assert!(headers.get("transfer-encoding").is_none());
    assert_eq!(headers.get("content-type").unwrap(), "application/json");
}

#[test]
fn sanitize_response_strips_x_oagw_headers() {
    let mut headers = HeaderMap::new();
    headers.insert("x-oagw-debug", "true".parse().unwrap());
    headers.insert("x-oagw-trace-id", "abc123".parse().unwrap());
    headers.insert("x-custom", "keep".parse().unwrap());

    sanitize_response_headers(&mut headers);

    assert!(headers.get("x-oagw-debug").is_none());
    assert!(headers.get("x-oagw-trace-id").is_none());
    assert_eq!(headers.get("x-custom").unwrap(), "keep");
}

// -- is_websocket_upgrade tests --

#[test]
fn websocket_upgrade_detected() {
    let mut headers = HeaderMap::new();
    headers.insert("upgrade", "websocket".parse().unwrap());
    headers.insert("connection", "Upgrade".parse().unwrap());
    assert!(is_websocket_upgrade(&headers));
}

#[test]
fn websocket_upgrade_case_insensitive() {
    let mut headers = HeaderMap::new();
    headers.insert("upgrade", "WebSocket".parse().unwrap());
    headers.insert("connection", "upgrade".parse().unwrap());
    assert!(is_websocket_upgrade(&headers));
}

#[test]
fn websocket_upgrade_multi_value() {
    let mut headers = HeaderMap::new();
    headers.insert("upgrade", "h2c, websocket".parse().unwrap());
    headers.insert("connection", "keep-alive, Upgrade".parse().unwrap());
    assert!(is_websocket_upgrade(&headers));
}

#[test]
fn websocket_upgrade_absent() {
    let headers = HeaderMap::new();
    assert!(!is_websocket_upgrade(&headers));
}

#[test]
fn websocket_upgrade_non_websocket() {
    let mut headers = HeaderMap::new();
    headers.insert("upgrade", "h2c".parse().unwrap());
    headers.insert("connection", "Upgrade".parse().unwrap());
    assert!(!is_websocket_upgrade(&headers));
}

#[test]
fn websocket_upgrade_missing_connection() {
    let mut headers = HeaderMap::new();
    headers.insert("upgrade", "websocket".parse().unwrap());
    assert!(!is_websocket_upgrade(&headers));
}

#[test]
fn websocket_upgrade_connection_without_upgrade_token() {
    let mut headers = HeaderMap::new();
    headers.insert("upgrade", "websocket".parse().unwrap());
    headers.insert("connection", "keep-alive".parse().unwrap());
    assert!(!is_websocket_upgrade(&headers));
}

#[test]
fn websocket_upgrade_multiple_header_instances() {
    let mut headers = HeaderMap::new();
    headers.append("upgrade", "h2c".parse().unwrap());
    headers.append("upgrade", "websocket".parse().unwrap());
    headers.append("connection", "keep-alive".parse().unwrap());
    headers.append("connection", "Upgrade".parse().unwrap());
    assert!(is_websocket_upgrade(&headers));
}

// -- strip_hop_by_hop_for_upgrade tests --

#[test]
fn upgrade_strip_preserves_upgrade_and_connection() {
    let mut headers = HeaderMap::new();
    headers.insert("upgrade", "websocket".parse().unwrap());
    headers.insert("connection", "Upgrade".parse().unwrap());
    headers.insert("transfer-encoding", "chunked".parse().unwrap());
    headers.insert("keep-alive", "timeout=5".parse().unwrap());
    headers.insert("x-custom", "keep".parse().unwrap());

    strip_hop_by_hop_for_upgrade(&mut headers);

    assert_eq!(headers.get("upgrade").unwrap(), "websocket");
    assert_eq!(headers.get("connection").unwrap(), "Upgrade");
    assert!(headers.get("transfer-encoding").is_none());
    assert!(headers.get("keep-alive").is_none());
    assert_eq!(headers.get("x-custom").unwrap(), "keep");
}

#[test]
fn upgrade_strip_removes_connection_nominated_except_upgrade() {
    let mut headers = HeaderMap::new();
    headers.insert("connection", "Upgrade, X-Custom-Hop".parse().unwrap());
    headers.insert("upgrade", "websocket".parse().unwrap());
    headers.insert("x-custom-hop", "secret".parse().unwrap());
    headers.insert("x-safe", "keep".parse().unwrap());

    strip_hop_by_hop_for_upgrade(&mut headers);

    assert!(headers.get("x-custom-hop").is_none());
    assert_eq!(headers.get("upgrade").unwrap(), "websocket");
    assert!(headers.get("connection").is_some());
    assert_eq!(headers.get("x-safe").unwrap(), "keep");
}

// -- sanitize_response_headers_for_upgrade tests --

#[test]
fn sanitize_upgrade_response_preserves_upgrade_strips_internal() {
    let mut headers = HeaderMap::new();
    headers.insert("upgrade", "websocket".parse().unwrap());
    headers.insert("connection", "Upgrade".parse().unwrap());
    headers.insert("x-oagw-debug", "true".parse().unwrap());
    headers.insert("x-custom", "keep".parse().unwrap());

    sanitize_response_headers_for_upgrade(&mut headers);

    assert_eq!(headers.get("upgrade").unwrap(), "websocket");
    assert_eq!(headers.get("connection").unwrap(), "Upgrade");
    assert!(headers.get("x-oagw-debug").is_none());
    assert_eq!(headers.get("x-custom").unwrap(), "keep");
}

#[test]
fn response_header_rules_set_add_remove() {
    let mut headers = HeaderMap::new();
    headers.insert("x-remove-me", "gone".parse().unwrap());
    headers.insert("x-overwrite", "old".parse().unwrap());
    headers.insert("content-type", "application/json".parse().unwrap());

    let rules = ResponseHeaderRules {
        set: [("x-overwrite".into(), "new".into())].into_iter().collect(),
        add: [("x-extra".into(), "added".into())].into_iter().collect(),
        remove: vec!["x-remove-me".into()],
    };

    apply_response_header_rules(&mut headers, &rules);

    assert!(headers.get("x-remove-me").is_none());
    assert_eq!(headers.get("x-overwrite").unwrap(), "new");
    assert_eq!(headers.get("x-extra").unwrap(), "added");
    assert_eq!(headers.get("content-type").unwrap(), "application/json");
}

#[test]
fn response_header_rules_empty_is_noop() {
    let mut headers = HeaderMap::new();
    headers.insert("x-keep", "value".parse().unwrap());

    let rules = ResponseHeaderRules::default();
    apply_response_header_rules(&mut headers, &rules);

    assert_eq!(headers.get("x-keep").unwrap(), "value");
}

#[test]
fn valid_content_type_accepted() {
    let mut headers = HeaderMap::new();
    headers.insert("content-type", "application/json".parse().unwrap());
    assert!(is_valid_content_type(&headers));
}

#[test]
fn valid_content_type_with_charset_accepted() {
    let mut headers = HeaderMap::new();
    headers.insert("content-type", "text/html; charset=utf-8".parse().unwrap());
    assert!(is_valid_content_type(&headers));
}

#[test]
fn missing_content_type_accepted() {
    let headers = HeaderMap::new();
    assert!(is_valid_content_type(&headers));
}

#[test]
fn invalid_content_type_rejected() {
    let mut headers = HeaderMap::new();
    headers.insert("content-type", "not a valid mime type!!!".parse().unwrap());
    assert!(!is_valid_content_type(&headers));
}

#[test]
fn transfer_encoding_absent_is_valid() {
    let headers = HeaderMap::new();
    assert!(is_valid_transfer_encoding(&headers));
}

#[test]
fn transfer_encoding_chunked_is_valid() {
    let mut headers = HeaderMap::new();
    headers.insert("transfer-encoding", "chunked".parse().unwrap());
    assert!(is_valid_transfer_encoding(&headers));
}

#[test]
fn transfer_encoding_chunked_case_insensitive() {
    let mut headers = HeaderMap::new();
    headers.insert("transfer-encoding", "Chunked".parse().unwrap());
    assert!(is_valid_transfer_encoding(&headers));
}

#[test]
fn transfer_encoding_gzip_rejected() {
    let mut headers = HeaderMap::new();
    headers.insert("transfer-encoding", "gzip".parse().unwrap());
    assert!(!is_valid_transfer_encoding(&headers));
}

#[test]
fn transfer_encoding_mixed_rejected() {
    let mut headers = HeaderMap::new();
    headers.insert("transfer-encoding", "gzip, chunked".parse().unwrap());
    assert!(!is_valid_transfer_encoding(&headers));
}

#[test]
fn duplicate_content_type_rejected() {
    let mut headers = HeaderMap::new();
    headers.append("content-type", "application/json".parse().unwrap());
    headers.append("content-type", "text/html".parse().unwrap());
    assert!(!is_valid_content_type(&headers));
}

#[test]
fn duplicate_transfer_encoding_rejected() {
    let mut headers = HeaderMap::new();
    headers.append("transfer-encoding", "chunked".parse().unwrap());
    headers.append("transfer-encoding", "chunked".parse().unwrap());
    assert!(!is_valid_transfer_encoding(&headers));
}
