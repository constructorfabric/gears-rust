use std::net::SocketAddr;

use toolkit_http_proxy::{ProtocolPolicy, Transport};

use super::*;
use crate::domain::model::{Endpoint, Scheme};

fn ep(host: &str, port: u16, scheme: Scheme) -> Endpoint {
    Endpoint {
        scheme,
        host: host.to_string(),
        port,
    }
}

fn addr(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}

#[test]
fn https_targets_negotiate_over_tls() {
    let t = target_for(&ep("api.example.com", 443, Scheme::Https), addr(443));
    assert_eq!(t.transport(), Transport::Tls);
    assert_eq!(t.policy(), ProtocolPolicy::Auto);
    assert_eq!(t.server_name(), "api.example.com");
    assert_eq!(t.authority(), "api.example.com");
}

#[test]
fn http_targets_are_cleartext_http1() {
    let t = target_for(&ep("api.example.com", 8080, Scheme::Http), addr(8080));
    assert_eq!(t.transport(), Transport::Plain);
    assert_eq!(t.authority(), "api.example.com:8080");
}

#[test]
fn wss_targets_use_http1_over_tls() {
    let t = target_for(&ep("ws.example.com", 443, Scheme::Wss), addr(443));
    assert_eq!(t.transport(), Transport::Tls);
    assert_eq!(t.policy(), ProtocolPolicy::Http1Only);
}

#[test]
fn wt_targets_negotiate_over_tls() {
    let t = target_for(&ep("wt.example.com", 443, Scheme::Wt), addr(443));
    assert_eq!(t.transport(), Transport::Tls);
    assert_eq!(t.policy(), ProtocolPolicy::Auto);
}

#[test]
fn authority_uses_the_endpoint_port_not_the_resolved_one() {
    let t = target_for(&ep("svc.internal", 443, Scheme::Https), addr(10443));
    assert_eq!(t.addr(), addr(10443));
    assert_eq!(t.authority(), "svc.internal");
}

#[test]
fn ssrf_filter_reports_reason() {
    let on = SsrfFilter(Arc::new(
        SsrfGuard::from_config(&Default::default()).unwrap(),
    ));
    assert!(on.check("example.com", &addr(80)).is_err());
    assert!(
        on.check("example.com", &"93.184.216.34:443".parse().unwrap())
            .is_ok()
    );
    let off = SsrfFilter(Arc::new(SsrfGuard::disabled()));
    assert!(off.check("example.com", &addr(80)).is_ok());
}

fn exempting(host: &str) -> SsrfGuard {
    SsrfGuard::from_config(&crate::domain::ssrf::SsrfPolicy {
        allow_hostnames: vec![host.into()],
        ..Default::default()
    })
    .unwrap()
}

/// The pool's filter honours an explicit hostname exemption for a private
/// address and still refuses the same address for any other name.
#[test]
fn ssrf_filter_honours_a_hostname_exemption() {
    let filter = SsrfFilter(Arc::new(exempting("trusted.internal")));
    let private: SocketAddr = "10.0.0.7:9999".parse().unwrap();
    assert!(filter.check("trusted.internal", &private).is_ok());
    assert!(filter.check("other.internal", &private).is_err());
}

/// The fallback honours an explicit hostname exemption and still refuses the
/// same address for any other name.
#[test]
fn fallback_honours_a_hostname_exemption() {
    let guard = exempting("trusted.internal");
    let private: SocketAddr = "10.0.0.7:9999".parse().unwrap();
    assert_eq!(
        first_permitted_addr(&guard, "trusted.internal", [private]),
        Ok(private)
    );
    assert!(matches!(
        first_permitted_addr(&guard, "other.internal", [private]),
        Err(NoPermittedAddr::Refused(_))
    ));
}

/// The fallback skips a refused address and takes the next permitted one;
/// with nothing resolved it says so rather than blaming the policy.
#[test]
fn fallback_takes_the_first_permitted_address() {
    let guard = exempting("trusted.internal");
    let metadata: SocketAddr = "169.254.169.254:80".parse().unwrap();
    let private: SocketAddr = "10.0.0.7:80".parse().unwrap();
    assert_eq!(
        first_permitted_addr(&guard, "trusted.internal", [metadata, private]),
        Ok(private)
    );
    assert_eq!(
        first_permitted_addr(&guard, "trusted.internal", []),
        Err(NoPermittedAddr::NoneResolved)
    );
    assert!(matches!(
        first_permitted_addr(&guard, "trusted.internal", [metadata]),
        Err(NoPermittedAddr::Refused(reason)) if reason.contains("allow_hostnames")
    ));
}

#[tokio::test]
async fn resolve_fallback_applies_ssrf_policy() {
    crate::domain::test_support::ensure_crypto_provider();
    let settings = EngineSettings {
        connect_timeout: Duration::from_secs(1),
        read_timeout: Duration::from_secs(1),
        protocol_cache_ttl: Duration::ZERO,
        #[cfg(feature = "test-utils")]
        skip_upstream_tls_verify: false,
    };
    let guarded = ProxyEngine::new(
        &settings,
        Arc::new(SsrfGuard::from_config(&Default::default()).unwrap()),
    )
    .unwrap();
    let selected = SelectedEndpoint {
        endpoint: ep("localhost", 9, Scheme::Https),
        resolved_addr: None,
    };
    let err = guarded.target(&selected, "/i").await.unwrap_err();
    assert!(
        matches!(err, DomainError::LinkUnavailable { .. }),
        "{err:?}"
    );

    let open = ProxyEngine::new(&settings, Arc::new(SsrfGuard::disabled())).unwrap();
    let t = open.target(&selected, "/i").await.unwrap();
    assert!(t.addr().ip().is_loopback());
}

type Check = fn(&DomainError) -> bool;

#[test]
fn error_mapping_matches_the_previous_engine() {
    use toolkit_http_proxy::ErrorKind as K;
    let cases: [(K, Check); 9] = [
        (K::ConnectTimeout, |e| {
            matches!(e, DomainError::ConnectionTimeout { .. })
        }),
        (K::ReadTimeout, |e| {
            matches!(e, DomainError::RequestTimeout { .. })
        }),
        (K::WriteTimeout, |e| {
            matches!(e, DomainError::RequestTimeout { .. })
        }),
        (K::Http2, |e| matches!(e, DomainError::ProtocolError { .. })),
        (K::Read, |e| matches!(e, DomainError::StreamAborted { .. })),
        (K::Connect, |e| {
            matches!(e, DomainError::LinkUnavailable { .. })
        }),
        (K::ConnectNoRoute, |e| {
            matches!(e, DomainError::LinkUnavailable { .. })
        }),
        (K::ConnectRefused, |e| {
            matches!(e, DomainError::DownstreamError { .. })
        }),
        (K::InvalidCertificate, |e| {
            matches!(e, DomainError::DownstreamError { .. })
        }),
    ];
    for (kind, check) in cases {
        let err = map_proxy_error(&test_error(kind), "/x");
        assert!(check(&err), "{kind:?} -> {err:?}");
    }
}

fn test_error(kind: toolkit_http_proxy::ErrorKind) -> ProxyError {
    toolkit_http_proxy::ProxyError::new(kind, "test")
}
