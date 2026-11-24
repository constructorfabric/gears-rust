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

/// A hostname resolving into a non-public IPv6 range is refused at connect
/// time, by the pool's filter and by the fallback resolution alike.
#[test]
fn resolved_addresses_in_non_public_ipv6_ranges_are_refused() {
    let guard = Arc::new(SsrfGuard::from_config(&Default::default()).unwrap());
    let filter = SsrfFilter(Arc::clone(&guard));
    let denied: [(SocketAddr, &str); 4] = [
        (
            "[4000::1]:443".parse().unwrap(),
            "address outside global unicast space (2000::/3)",
        ),
        (
            "[5f00::1]:443".parse().unwrap(),
            "SRv6 segment identifier (5f00::/16)",
        ),
        (
            "[fec0::1]:443".parse().unwrap(),
            "site-local address (fec0::/10)",
        ),
        (
            "[64:ff9b:1::a00:1]:443".parse().unwrap(),
            "local-use NAT64 address (64:ff9b:1::/48)",
        ),
    ];
    for (addr, reason) in denied {
        assert_eq!(
            filter.check("api.example.com", &addr),
            Err(reason),
            "{addr}"
        );
    }

    let public: SocketAddr = "[2606:4700::1111]:443".parse().unwrap();
    assert_eq!(filter.check("api.example.com", &public), Ok(()));
    assert_eq!(
        first_permitted_addr(
            &guard,
            "api.example.com",
            denied.iter().map(|(a, _)| *a).chain([public])
        ),
        Ok(public)
    );
    assert_eq!(
        first_permitted_addr(&guard, "api.example.com", denied.iter().map(|(a, _)| *a)),
        Err(NoPermittedAddr::Refused(
            "local-use NAT64 address (64:ff9b:1::/48)".to_owned()
        ))
    );
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
        matches!(
            err,
            TargetError::Refused(DomainError::LinkUnavailable { .. })
        ),
        "{err:?}"
    );

    let open = ProxyEngine::new(&settings, Arc::new(SsrfGuard::disabled())).unwrap();
    let t = open.target(&selected, "/i").await.unwrap();
    assert!(t.addr().ip().is_loopback());

    // A name that does not resolve is the upstream's problem, not the policy's.
    let unresolvable = SelectedEndpoint {
        endpoint: ep("oagw-test.invalid", 443, Scheme::Https),
        resolved_addr: None,
    };
    let err = guarded.target(&unresolvable, "/i").await.unwrap_err();
    assert!(
        matches!(
            err,
            TargetError::Unreachable(DomainError::LinkUnavailable { .. })
        ),
        "{err:?}"
    );
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

/// Every engine error kind, its wire status and its dispatch reason
/// (#4985). The status follows the `DomainError` variant; the reason comes
/// from the kind alone. `ErrorKind` is `#[non_exhaustive]`, so the table is
/// checked against `ErrorKind::ALL` instead of by the compiler: a kind the
/// engine adds fails here until its status and dispatch stage are chosen.
#[test]
fn every_error_kind_has_a_status_and_a_dispatch_reason() {
    use oagw_sdk::reason::unavailable::{
        REQUEST_MAY_HAVE_BEEN_SENT as SENT, REQUEST_NOT_SENT as NOT,
    };
    use toolkit_canonical_errors::{CanonicalError, Problem};
    use toolkit_http_proxy::ErrorKind as K;

    let table: [(K, u16, Option<&str>); 16] = [
        (K::ConnectTimeout, 504, Some(NOT)),
        (K::ConnectRefused, 502, Some(NOT)),
        (K::ConnectNoRoute, 503, Some(NOT)),
        (K::Connect, 503, Some(NOT)),
        (K::TlsHandshakeTimeout, 502, Some(NOT)),
        (K::TlsHandshake, 502, Some(NOT)),
        (K::InvalidCertificate, 502, Some(NOT)),
        (K::ReadTimeout, 504, Some(SENT)),
        (K::WriteTimeout, 504, Some(SENT)),
        (K::ConnectionClosed, 502, Some(SENT)),
        (K::Read, 502, Some(SENT)),
        (K::Write, 502, Some(SENT)),
        (K::Http2, 502, Some(SENT)),
        (K::Protocol, 502, Some(SENT)),
        // Refused before sending; not a transport failure, no reason.
        (K::InvalidRequest, 400, None),
        (K::Configuration, 500, None),
    ];
    let listed: std::collections::HashSet<_> = table.iter().map(|(k, ..)| *k).collect();
    assert_eq!(listed.len(), table.len(), "each kind once");
    let all: std::collections::HashSet<_> = K::ALL.iter().copied().collect();
    assert_eq!(listed, all, "the table covers exactly ErrorKind::ALL");

    for (kind, status, reason) in table {
        let err = map_proxy_error(&test_error(kind), "/x");
        let problem = Problem::from(CanonicalError::from(err));
        assert_eq!(problem.status, Some(status), "{kind:?}");
        assert_eq!(
            problem.context.get("reason").and_then(|r| r.as_str()),
            reason,
            "{kind:?}"
        );
    }
}

/// The engine resent the request after a first attempt the upstream may
/// have processed: whatever the resend failed with, a connect failure
/// included, the reason is `REQUEST_MAY_HAVE_BEEN_SENT`.
#[test]
fn a_failed_resend_after_a_possibly_processed_attempt_may_have_been_sent() {
    use oagw_sdk::reason::unavailable::REQUEST_MAY_HAVE_BEEN_SENT;
    use toolkit_canonical_errors::{CanonicalError, Problem};
    use toolkit_http_proxy::ErrorKind as K;

    for kind in [
        K::ConnectRefused,
        K::ConnectTimeout,
        K::Connect,
        K::TlsHandshake,
        K::ConnectionClosed,
        K::ReadTimeout,
    ] {
        let err = test_error(kind).after_attempt_that_may_have_been_processed(true);
        let problem = Problem::from(CanonicalError::from(map_proxy_error(&err, "/x")));
        assert_eq!(
            problem.context.get("reason").and_then(|r| r.as_str()),
            Some(REQUEST_MAY_HAVE_BEEN_SENT),
            "{kind:?}"
        );
    }
}

fn test_error(kind: toolkit_http_proxy::ErrorKind) -> ProxyError {
    toolkit_http_proxy::ProxyError::new(kind, "test")
}

/// Connect and TLS failures happen before any of the request is sent; the
/// rest can follow a client's slow upload.
#[test]
fn connect_failures_are_told_from_exchange_failures() {
    use toolkit_http_proxy::ErrorKind as K;
    for kind in [
        K::ConnectTimeout,
        K::ConnectRefused,
        K::ConnectNoRoute,
        K::Connect,
        K::TlsHandshakeTimeout,
        K::TlsHandshake,
        K::InvalidCertificate,
    ] {
        assert!(is_connect_failure(&test_error(kind)), "{kind:?}");
    }
    for kind in [
        K::ReadTimeout,
        K::WriteTimeout,
        K::ConnectionClosed,
        K::Read,
        K::Write,
        K::Http2,
        K::Protocol,
        // Refused by the engine: no connection was attempted.
        K::InvalidRequest,
        K::Configuration,
    ] {
        assert!(!is_connect_failure(&test_error(kind)), "{kind:?}");
    }
}
