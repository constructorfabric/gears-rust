use super::*;

#[test]
fn accepts_well_formed_https_domain() {
    let url = validate_outbound_url("https://api.example.com/v1", "endpoint").expect("ok");
    assert_eq!(url.host_str(), Some("api.example.com"));
}

#[test]
fn rejects_empty_string() {
    let err = validate_outbound_url("   ", "endpoint").unwrap_err();
    assert!(matches!(err, PluginError::InvalidInput { .. }));
    assert!(err.to_string().contains("endpoint"));
}

#[test]
fn rejects_bare_path() {
    // Non-absolute — would otherwise be sent as a relative POST against
    // the reqwest client's base URL (here: nothing → panic at send
    // time). Catching at parse-time gives a typed config error instead.
    let err = validate_outbound_url("/internal/api", "endpoint").unwrap_err();
    assert!(matches!(err, PluginError::InvalidInput { .. }));
}

#[test]
fn rejects_http_scheme() {
    let err = validate_outbound_url("http://api.example.com/v1", "endpoint").unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("https"),
        "must surface scheme requirement: {msg}"
    );
}

#[test]
fn rejects_data_scheme() {
    let err = validate_outbound_url("data:application/json,{}", "endpoint").unwrap_err();
    assert!(matches!(err, PluginError::InvalidInput { .. }));
}

#[test]
fn rejects_localhost_hostname() {
    let err = validate_outbound_url("https://localhost/path", "endpoint").unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.to_ascii_lowercase().contains("localhost"),
        "must call out localhost: {msg}",
    );
}

#[test]
fn rejects_localhost_subdomain() {
    // `*.localhost` resolves to the loopback per RFC 6761; the parser
    // accepts it as a domain so we must catch it explicitly.
    let err = validate_outbound_url("https://api.localhost/v1", "endpoint").unwrap_err();
    assert!(matches!(err, PluginError::InvalidInput { .. }));
}

#[test]
fn rejects_loopback_ipv4() {
    let err = validate_outbound_url("https://127.0.0.1/x", "endpoint").unwrap_err();
    assert!(err.to_string().contains("IPv4"));
}

#[test]
fn rejects_cloud_metadata_ip() {
    // 169.254.169.254 — AWS / GCP / Azure IMDS endpoint. Must NEVER
    // be reachable through a tenant-configured URL.
    let err =
        validate_outbound_url("https://169.254.169.254/latest/meta-data/", "endpoint").unwrap_err();
    assert!(matches!(err, PluginError::InvalidInput { .. }));
}

#[test]
fn rejects_private_rfc1918_ipv4_ranges() {
    for raw in [
        "https://10.0.0.1/x",
        "https://10.255.255.255/x",
        "https://172.16.0.1/x",
        "https://172.31.255.254/x",
        "https://192.168.1.1/x",
    ] {
        let err = validate_outbound_url(raw, "endpoint").unwrap_err_or_else_helper(raw);
        assert!(matches!(err, PluginError::InvalidInput { .. }), "{raw}");
    }
}

#[test]
fn rejects_zero_network_ipv4() {
    let err = validate_outbound_url("https://0.0.0.0/x", "endpoint").unwrap_err();
    assert!(matches!(err, PluginError::InvalidInput { .. }));
}

#[test]
fn rejects_multicast_ipv4() {
    let err = validate_outbound_url("https://224.0.0.1/x", "endpoint").unwrap_err();
    assert!(matches!(err, PluginError::InvalidInput { .. }));
}

#[test]
fn rejects_cgnat_ipv4() {
    let err = validate_outbound_url("https://100.64.0.1/x", "endpoint").unwrap_err();
    assert!(matches!(err, PluginError::InvalidInput { .. }));
}

#[test]
fn rejects_loopback_ipv6() {
    let err = validate_outbound_url("https://[::1]/x", "endpoint").unwrap_err();
    assert!(matches!(err, PluginError::InvalidInput { .. }));
}

#[test]
fn rejects_unique_local_ipv6() {
    let err = validate_outbound_url("https://[fd12:3456:789a::1]/x", "endpoint").unwrap_err();
    assert!(matches!(err, PluginError::InvalidInput { .. }));
}

#[test]
fn rejects_link_local_ipv6() {
    let err = validate_outbound_url("https://[fe80::1]/x", "endpoint").unwrap_err();
    assert!(matches!(err, PluginError::InvalidInput { .. }));
}

#[test]
fn rejects_ipv4_mapped_loopback_in_ipv6() {
    // `::ffff:127.0.0.1` is an IPv4-mapped form of loopback; without
    // explicit handling this would slip past `IpAddr::is_loopback()`
    // because the IPv6 representation is NOT `::1`.
    let err = validate_outbound_url("https://[::ffff:127.0.0.1]/x", "endpoint").unwrap_err();
    assert!(matches!(err, PluginError::InvalidInput { .. }));
}

#[test]
fn accepts_global_public_ipv4() {
    // 8.8.8.8 — a clearly public IP. We don't want to over-block.
    validate_outbound_url("https://8.8.8.8/v1", "endpoint").expect("public IPs are allowed");
}

#[test]
fn key_name_appears_in_error_messages() {
    let err = validate_outbound_url("http://localhost/x", "gateway_url").unwrap_err();
    assert!(err.to_string().contains("gateway_url"));
}

// --- internal test helpers ------------------------------------------

trait UnwrapErrOr<T, E> {
    fn unwrap_err_or_else_helper(self, ctx: &str) -> E;
}
impl<T: std::fmt::Debug, E> UnwrapErrOr<T, E> for Result<T, E> {
    fn unwrap_err_or_else_helper(self, ctx: &str) -> E {
        match self {
            Ok(v) => panic!("expected Err for {ctx}, got Ok({v:?})"),
            Err(e) => e,
        }
    }
}
