#![cfg(feature = "fips")]

//! Regression for issue #1934: under `--features fips`, every non-testing
//! [`HttpClientConfig`] preset must default to [`TransportSecurity::TlsOnly`],
//! and [`HttpClientBuilder::build`] must reject [`TransportSecurity::AllowInsecureHttp`]
//! before any TLS work runs.
//!
//! Lives in its own integration-test binary (separate process from the
//! `cargo test` in-crate unit tests) so the FIPS-guard path can be exercised
//! without depending on the in-crate `fips_test_provider` auto-install — the
//! [`HttpError::InsecureTransport`] check fires at the top of `build()`,
//! before any rustls `CryptoProvider` is consulted.

use modkit_http::{
    HttpClient, HttpClientBuilder, HttpClientConfig, HttpError, RedirectConfig, TransportSecurity,
};

/// Acceptance criterion 1:
/// `HttpClientConfig::default().transport == TransportSecurity::TlsOnly`
/// under `--features fips`.
#[test]
fn default_preset_is_tls_only() {
    assert_eq!(
        HttpClientConfig::default().transport,
        TransportSecurity::TlsOnly,
        "default() must default to TlsOnly under --features fips"
    );
}

/// Every non-testing preset must flip to `TlsOnly` so a caller picking up any
/// preset under FIPS gets a secure default.
#[test]
fn all_non_testing_presets_are_tls_only() {
    let presets: [(&str, TransportSecurity); 5] = [
        ("default", HttpClientConfig::default().transport),
        ("minimal", HttpClientConfig::minimal().transport),
        ("infra_default", HttpClientConfig::infra_default().transport),
        (
            "token_endpoint",
            HttpClientConfig::token_endpoint().transport,
        ),
        ("sse", HttpClientConfig::sse().transport),
    ];

    for (name, transport) in presets {
        assert_eq!(
            transport,
            TransportSecurity::TlsOnly,
            "preset {name}() must default to TlsOnly under --features fips"
        );
    }
}

/// Acceptance criterion 3:
/// `for_testing()` is documented as the only preset that carries
/// `AllowInsecureHttp`. The preset is intended for non-FIPS test code;
/// under FIPS, attempting to build a client from it still fails closed via
/// the validation tested by [`build_rejects_allow_insecure_http`].
#[test]
fn for_testing_preset_field_keeps_allow_insecure_http() {
    assert_eq!(
        HttpClientConfig::for_testing().transport,
        TransportSecurity::AllowInsecureHttp,
        "for_testing() must keep AllowInsecureHttp so non-fips tests can run"
    );
}

/// Acceptance criterion 2:
/// `HttpClient::builder().transport(AllowInsecureHttp).build()` returns
/// `Err(HttpError::InsecureTransport)` under `--features fips`.
///
/// The FIPS guard runs before any TLS configuration is built, so this test
/// does not need a rustls `CryptoProvider` to be installed. That is also why
/// it can coexist with `tests/no_crypto_provider_fips.rs` (which asserts the
/// `NoCryptoProvider` path in a separate test binary) without either test
/// disturbing the other.
#[test]
fn build_rejects_allow_insecure_http() {
    let Err(err) = HttpClient::builder()
        .transport(TransportSecurity::AllowInsecureHttp)
        .build()
    else {
        panic!(
            "HttpClient::builder().transport(AllowInsecureHttp).build() must fail \
             under --features fips"
        );
    };

    assert!(
        matches!(err, HttpError::InsecureTransport),
        "expected HttpError::InsecureTransport, got {err:?}"
    );
}

/// Same guard must also fire when `AllowInsecureHttp` is carried in via the
/// `for_testing()` preset — there is no escape hatch under FIPS.
#[test]
fn build_rejects_for_testing_preset() {
    let mut config = HttpClientConfig::for_testing();
    // Defensive: assert the preset still carries plaintext before we test
    // that build() rejects it. If this ever flips, this test is no longer
    // meaningful and should be revisited.
    assert_eq!(config.transport, TransportSecurity::AllowInsecureHttp);
    // Touch an unrelated field so the test stays robust against future
    // additions to the preset.
    config.redirect = RedirectConfig::disabled();

    let Err(err) = HttpClientBuilder::with_config(config).build() else {
        panic!("for_testing() preset must be rejected at build() under --features fips");
    };

    assert!(
        matches!(err, HttpError::InsecureTransport),
        "expected HttpError::InsecureTransport, got {err:?}"
    );
}
