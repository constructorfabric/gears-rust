//! FIPS 140-3 mode: the engine builds only FIPS-reporting TLS configs and
//! completes real handshakes on the FIPS provider.
//!
//! Run with `cargo nextest run -p cf-gears-toolkit-http-proxy --features fips`
//! (Linux: AWS-LC FIPS; see `make test-fips`).

#![cfg(all(feature = "fips", not(any(target_os = "macos", target_os = "windows"))))]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use http::Request;
use support::{Wire, body_string, echo, test_config};
use toolkit_http_proxy::{ProxyClient, empty_body};

#[test]
fn installed_provider_is_fips() {
    support::install_provider();
    let provider = rustls::crypto::CryptoProvider::get_default().expect("provider");
    assert!(provider.fips(), "the process provider must be the FIPS one");
}

#[tokio::test]
async fn engine_builds_and_handshakes_in_fips_mode() {
    // Construction runs `ClientConfig::fips()` on every ALPN variant and
    // fails closed otherwise.
    let client = ProxyClient::new(test_config()).expect("FIPS engine");
    for wire in [Wire::TlsH1, Wire::TlsH2, Wire::TlsBoth] {
        let up = echo(wire).await;
        let resp = client
            .send(
                &up.target(wire),
                Request::get("/fips").body(empty_body()).unwrap(),
            )
            .await
            .expect("FIPS handshake");
        assert!(body_string(resp).await.starts_with("GET"), "{wire:?}");
    }
}

#[test]
fn certificate_verification_cannot_be_disabled() {
    let mut cfg = test_config();
    cfg.tls.verification = toolkit_http_proxy::CertVerification::DangerAcceptAny;
    assert!(ProxyClient::new(cfg).is_err());
}
