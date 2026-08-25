//! Tests for the HTTP-to-domain error mapping.

use std::time::Duration;

use bss_ledger_sdk::RateProviderError;
use toolkit_http::{HttpError, InvalidUriKind};

use super::{error_kind_label, map_http_error};

#[test]
fn timeout_maps_to_unreachable() {
    let mapped = map_http_error(&HttpError::Timeout(Duration::from_secs(5)));
    assert!(matches!(mapped, RateProviderError::Unreachable(_)));
}

#[test]
fn deadline_maps_to_unreachable() {
    let mapped = map_http_error(&HttpError::DeadlineExceeded(Duration::from_secs(5)));
    assert!(matches!(mapped, RateProviderError::Unreachable(_)));
}

#[test]
fn overloaded_maps_to_unreachable() {
    assert!(matches!(
        map_http_error(&HttpError::Overloaded),
        RateProviderError::Unreachable(_)
    ));
}

#[test]
fn http_status_maps_to_upstream_status() {
    let mapped = map_http_error(&HttpError::HttpStatus {
        status: http::StatusCode::SERVICE_UNAVAILABLE,
        body_preview: String::new(),
        content_type: None,
        retry_after: None,
    });
    assert!(matches!(mapped, RateProviderError::UpstreamStatus(503)));
}

#[test]
fn invalid_uri_maps_to_internal_and_never_echoes_the_raw_url() {
    // A `base_url` may have an operator-embedded secret spliced into it (no
    // source supports query-string auth) — the mapped error must never repeat
    // the raw URL verbatim, only the structured `kind` + diagnostic `reason`.
    let secret_url = "https://api.example.com/latest?access_key=SECRET123";
    let mapped = map_http_error(&HttpError::InvalidUri {
        url: secret_url.to_owned(),
        kind: InvalidUriKind::ParseError,
        reason: "unexpected character".to_owned(),
    });
    match mapped {
        RateProviderError::Internal(msg) => {
            assert!(
                !msg.contains("SECRET123"),
                "mapped error leaked the raw URL: {msg}"
            );
        }
        other => panic!("expected Internal, got {other:?}"),
    }
}

#[test]
fn error_kind_label_reflects_the_mapped_variant_not_a_hardcoded_guess() {
    assert_eq!(
        error_kind_label(&RateProviderError::Unreachable("x".to_owned())),
        "unreachable"
    );
    assert_eq!(
        error_kind_label(&RateProviderError::UpstreamStatus(503)),
        "upstream_status"
    );
    assert_eq!(
        error_kind_label(&RateProviderError::Internal("x".to_owned())),
        "internal"
    );
    assert_eq!(
        error_kind_label(&RateProviderError::InvalidPair("x".to_owned())),
        "invalid_pair"
    );
    assert_eq!(
        error_kind_label(&RateProviderError::PairUnavailable {
            base: "EUR".to_owned(),
            quote: "USD".to_owned()
        }),
        "pair_unavailable"
    );
}
