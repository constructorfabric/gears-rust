use std::time::Duration;

use modkit::client_hub::{ClientHub, ClientHubError};
use types_registry_sdk::{TypesRegistryClient, TypesRegistryError};
use usage_collector_sdk::UsageRecordError;

use super::*;

/// Variant tag for table-driven assertions about which canonical arm a domain
/// error is mapped to.
#[derive(Debug, PartialEq, Eq)]
enum CanonicalKind {
    NotFound,
    DeadlineExceeded,
    ServiceUnavailable,
    Internal,
    ResourceExhausted,
}

fn kind_of(err: &UsageCollectorError) -> CanonicalKind {
    match err {
        UsageCollectorError::NotFound { .. } => CanonicalKind::NotFound,
        UsageCollectorError::DeadlineExceeded { .. } => CanonicalKind::DeadlineExceeded,
        UsageCollectorError::ServiceUnavailable { .. } => CanonicalKind::ServiceUnavailable,
        UsageCollectorError::Internal { .. } => CanonicalKind::Internal,
        UsageCollectorError::ResourceExhausted { .. } => CanonicalKind::ResourceExhausted,
        other => panic!("unexpected canonical variant: {other:?}"),
    }
}

/// `ClientHubError`'s only public-constructible variants come from the hub itself
/// (the `TypeKey` field has no public constructor); produce one by querying an
/// empty hub.
fn sample_client_hub_error() -> ClientHubError {
    ClientHub::default()
        .get::<dyn TypesRegistryClient>()
        .err()
        .expect("empty hub must return ClientHubError::NotFound")
}

/// Canonical `Internal` variant scrubs the caller-supplied detail and replaces it with
/// a fixed sanitized string, so detail assertions on `Internal`-mapped arms must compare
/// against that fixed text rather than the upstream message.
const INTERNAL_DETAIL: &str = "An internal error occurred. Please retry later.";

#[test]
fn from_domain_error_table() {
    let cases: Vec<(DomainError, CanonicalKind, &str)> = vec![
        (
            DomainError::TypesRegistryUnavailable(TypesRegistryError::Internal(
                "registry offline".to_owned(),
            )),
            CanonicalKind::ServiceUnavailable,
            "types registry is not available: Internal error: registry offline",
        ),
        (
            DomainError::ClientHub(sample_client_hub_error()),
            CanonicalKind::Internal,
            INTERNAL_DETAIL,
        ),
        (
            DomainError::ModuleNotConfigured {
                module: "billing".to_owned(),
            },
            CanonicalKind::NotFound,
            "module not configured",
        ),
        (
            DomainError::Timeout,
            CanonicalKind::DeadlineExceeded,
            "plugin call timed out",
        ),
        (
            DomainError::CircuitOpen,
            CanonicalKind::ServiceUnavailable,
            "circuit breaker open",
        ),
        (
            DomainError::PluginNotFound {
                vendor: "acme".to_owned(),
            },
            CanonicalKind::ServiceUnavailable,
            "no plugin instances found for vendor 'acme'",
        ),
        (
            DomainError::PluginUnavailable {
                gts_id: "gts.x.v1~inst".to_owned(),
                reason: "client missing".to_owned(),
            },
            CanonicalKind::ServiceUnavailable,
            "plugin not available for 'gts.x.v1~inst': client missing",
        ),
        (
            DomainError::InvalidPluginInstance {
                gts_id: "gts.x.v1~inst".to_owned(),
                reason: "missing field".to_owned(),
            },
            CanonicalKind::Internal,
            INTERNAL_DETAIL,
        ),
        (
            DomainError::Internal("boom".to_owned()),
            CanonicalKind::Internal,
            INTERNAL_DETAIL,
        ),
    ];

    for (input, expected_kind, expected_detail) in cases {
        let label = format!("{input:?}");
        let canonical: UsageCollectorError = input.into();
        assert_eq!(
            kind_of(&canonical),
            expected_kind,
            "variant mismatch for case {label}",
        );
        assert!(
            canonical.detail().contains(expected_detail),
            "detail mismatch for case {label}: got '{}', expected to contain '{expected_detail}'",
            canonical.detail(),
        );
    }
}

#[test]
fn from_domain_plugin_preserves_canonical() {
    let canonical = UsageRecordError::resource_exhausted("rows exceed limit")
        .with_quota_violation("rows", "too many")
        .create();
    let mapped: UsageCollectorError = DomainError::Plugin(canonical).into();
    assert_eq!(kind_of(&mapped), CanonicalKind::ResourceExhausted);
}

#[test]
fn from_types_registry_error_carries_source() {
    let inner = TypesRegistryError::ServiceUnavailable {
        message: "still initializing".to_owned(),
        retry_after: Duration::from_secs(1),
    };
    let domain: DomainError = inner.into();
    let source = std::error::Error::source(&domain).expect("source chain must be preserved");
    assert!(source.to_string().contains("still initializing"));
}

#[test]
fn from_client_hub_error_carries_source() {
    let inner = sample_client_hub_error();
    let inner_msg = inner.to_string();
    let domain: DomainError = inner.into();
    let source = std::error::Error::source(&domain).expect("source chain must be preserved");
    assert_eq!(source.to_string(), inner_msg);
}
