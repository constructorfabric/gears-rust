//! Tests for the shared outbound-client gate: https-only with a host, a usable
//! timeout, and never echoing the (possibly secret-bearing) `base_url` into the
//! error.

use super::{MAX_TIMEOUT_MS, build_source_http_client};

// The accepting cases are async because building the client spawns a `tower`
// buffer worker, which needs a live Tokio reactor. The rejection cases fail
// validation before any client is built, so they stay plain `#[test]`.
#[tokio::test]
async fn https_base_url_builds() {
    assert!(build_source_http_client("https://example.com/rates", 1000).is_ok());
}

#[tokio::test]
async fn scheme_is_matched_case_insensitively() {
    // A URI scheme is case-insensitive (RFC 3986 §3.1) and `http::Uri`
    // canonicalizes it, so `toolkit-http`'s own TLS gate sees plain `https` and
    // uses TLS. Rejecting these here would fail a configuration that works.
    for url in [
        "HTTPS://example.com/rates",
        "HttPs://example.com/rates",
        "Https://example.com/rates",
    ] {
        assert!(
            build_source_http_client(url, 1000).is_ok(),
            "{url:?} is valid https and must be accepted"
        );
    }
}

#[test]
fn plain_http_is_rejected_at_build_time() {
    // Validating at init rather than at first fetch: a plain-http
    // misconfiguration must fail loudly instead of silently sending rates over
    // cleartext. Upper-case too — casing must not bypass the scheme check.
    for url in ["http://example.com/rates", "HTTP://example.com/rates"] {
        assert!(
            build_source_http_client(url, 1000).is_err(),
            "{url:?} must be rejected"
        );
    }
}

#[test]
fn other_schemes_are_rejected() {
    for url in [
        "ftp://example.com/rates",
        "file:///etc/passwd",
        "wss://example.com/rates",
        // No scheme at all.
        "example.com/rates",
        "",
    ] {
        assert!(
            build_source_http_client(url, 1000).is_err(),
            "{url:?} must be rejected"
        );
    }
}

#[test]
fn https_without_a_host_is_rejected() {
    // The right scheme but no reachable authority, so not a usable endpoint.
    for url in ["https://", "https:///rates", "https:/example.com"] {
        assert!(
            build_source_http_client(url, 1000).is_err(),
            "{url:?} has no host and must be rejected"
        );
    }
}

// Async unlike the other rejection cases: if the zero-timeout gate is ever
// removed, this test then reaches the client build, and a reactor keeps that
// failure a clean assertion instead of a confusing "no reactor running" panic.
#[tokio::test]
async fn zero_timeout_is_rejected() {
    // A zero `timeout_ms` is not "unlimited": it becomes a zero-duration
    // per-attempt deadline that elapses before any connect completes, so every
    // fetch (and every retry) fails at the first sync tick.
    assert!(
        build_source_http_client("https://example.com/rates", 0).is_err(),
        "a zero timeout must be rejected"
    );
}

#[tokio::test]
async fn smallest_nonzero_timeout_is_accepted() {
    // Above zero the lower end stays the operator's choice.
    assert!(build_source_http_client("https://example.com/rates", 1).is_ok());
}

// The two tests below pin the ceiling as a pair: either alone would pass for an
// arbitrary bound.
#[tokio::test]
async fn timeout_at_the_ceiling_is_accepted() {
    assert!(
        build_source_http_client("https://example.com/rates", MAX_TIMEOUT_MS).is_ok(),
        "exactly MAX_TIMEOUT_MS must still build: the bound is inclusive"
    );
}

#[tokio::test]
async fn timeout_above_the_ceiling_is_rejected() {
    // Sources are fetched sequentially, so one oversized timeout eats into the
    // whole rate-sync pass. Both a hair past the ceiling and the realistic unit
    // mix-up (seconds written into a milliseconds field) are rejected.
    for timeout_ms in [MAX_TIMEOUT_MS + 1, 3_600_000] {
        assert!(
            build_source_http_client("https://example.com/rates", timeout_ms).is_err(),
            "{timeout_ms} ms exceeds MAX_TIMEOUT_MS and must be rejected"
        );
    }
}

#[test]
fn oversized_timeout_error_names_the_ceiling_and_not_the_url() {
    // The operator has to learn the limit from the message; the message is
    // logged, so it must still not repeat the (possibly secret-bearing) URL.
    let secret_url = "https://api.example.com/latest?access_key=SECRET123";
    let Err(err) = build_source_http_client(secret_url, MAX_TIMEOUT_MS + 1) else {
        panic!("an oversized timeout must be rejected");
    };
    let msg = format!("{err:#}");
    assert!(
        msg.contains(&MAX_TIMEOUT_MS.to_string()),
        "error must name the ceiling: {msg}"
    );
    assert!(
        !msg.contains("SECRET123") && !msg.contains("api.example.com"),
        "error leaked the URL: {msg}"
    );
}

#[test]
fn rejection_error_never_echoes_the_url() {
    // A `base_url` may carry an operator-embedded credential; the error is logged
    // by the sync job, so it must not repeat the URL verbatim. (`HttpClient` isn't
    // `Debug`, so unwrap the error by matching, not `unwrap_err`.)
    let secret_url = "http://api.example.com/latest?access_key=SECRET123";
    let Err(err) = build_source_http_client(secret_url, 1000) else {
        panic!("plain http must be rejected");
    };
    let msg = format!("{err:#}");
    assert!(
        !msg.contains("SECRET123"),
        "error leaked the credential: {msg}"
    );
    assert!(
        !msg.contains("api.example.com"),
        "error leaked the host: {msg}"
    );
}

#[test]
fn unparseable_url_error_never_echoes_the_url() {
    // The parse-failure path formats the underlying `http` error, so cover it
    // separately from the scheme-rejection path above.
    let secret_url = "ht tp://api.example.com/latest?access_key=SECRET123";
    let Err(err) = build_source_http_client(secret_url, 1000) else {
        panic!("a malformed URL must be rejected");
    };
    let msg = format!("{err:#}");
    assert!(
        !msg.contains("SECRET123"),
        "error leaked the credential: {msg}"
    );
    assert!(
        !msg.contains("api.example.com"),
        "error leaked the host: {msg}"
    );
}
