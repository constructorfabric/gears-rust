//! SDK-level unit tests for the service-account contract types.
//!
//! Cover the surface the SDK owns alone: the secret wrapper on the
//! credential shape, metric-label stability, uniform `detail()` reads,
//! and the serde round-trip on the published listing entry. Provider
//! behaviour is exercised at the impl-side seams (AM
//! `ServiceAccountService` against `FakeServiceAccountProvider`).

#![allow(clippy::expect_used, clippy::unwrap_used, reason = "test helpers")]

use secrecy::ExposeSecret as _;
use toolkit_gts::gts_id;

use super::*;

fn tenant_context() -> IdpTenantContext {
    IdpTenantContext::new(
        Uuid::from_u128(1),
        "acme",
        gts::GtsTypeId::new(gts_id!("cf.core.am.tenant_type.v1~cf.core.am.customer.v1~")),
        None,
    )
}

/// The secret is the one field that must never reach a log line through
/// a `Debug`-formatted request/response. `SecretString` redacts it; this
/// pins that the wrapper is actually in place on the struct field.
#[test]
fn credentials_debug_hides_secret() {
    let creds = IdpServiceAccountCredentials::new(
        "svc-abc".to_owned(),
        SecretString::from("super-secret".to_owned()),
        "https://example.com/token".to_owned(),
        Uuid::nil(),
    );
    assert!(!format!("{creds:?}").contains("super-secret"));
    assert_eq!(creds.client_secret.expose_secret(), "super-secret");
}

/// Metric labels are an observability contract: a rename silently
/// re-buckets every dashboard and alert keyed on the old value.
#[test]
fn metric_labels_are_stable() {
    let cases = [
        (
            IdpServiceAccountFailure::InvalidInput {
                detail: String::new(),
                field: None,
            },
            "invalid_input",
        ),
        (
            IdpServiceAccountFailure::NotFound {
                detail: String::new(),
            },
            "not_found",
        ),
        (
            IdpServiceAccountFailure::CleanFailure {
                detail: String::new(),
            },
            "clean_failure",
        ),
        (
            IdpServiceAccountFailure::Ambiguous {
                detail: String::new(),
            },
            "ambiguous",
        ),
        (
            IdpServiceAccountFailure::UnsupportedOperation {
                detail: String::new(),
            },
            "unsupported_operation",
        ),
    ];
    for (failure, label) in cases {
        assert_eq!(failure.as_metric_label(), label);
        assert_eq!(failure.detail(), "");
    }
}

#[test]
fn display_and_debug_expose_only_the_stable_label() {
    let failure = IdpServiceAccountFailure::InvalidInput {
        detail: "secret=must-not-appear".to_owned(),
        field: Some("secret-field-must-not-appear".to_owned()),
    };
    let displayed = failure.to_string();
    let debugged = format!("{failure:?}");
    assert_eq!(displayed, "invalid_input");
    assert_eq!(debugged, "invalid_input");
    for rendered in [&displayed, &debugged] {
        assert!(!rendered.contains("must-not-appear"));
    }
}

/// `detail()` reads uniformly across variants — including the one that
/// carries a second field — so AM's boundary can take the length of the
/// discarded text without matching per variant.
#[test]
fn detail_reads_through_every_variant() {
    let failure = IdpServiceAccountFailure::InvalidInput {
        detail: "name taken".to_owned(),
        field: Some("name".to_owned()),
    };
    assert_eq!(failure.detail(), "name taken");
}

/// The request shapes carry the AM-resolved tenant context, which is
/// what makes the plugin's per-tenant `metadata` available on every
/// service-account call (symmetric with the user-ops requests).
#[test]
fn requests_carry_the_resolved_tenant_context() {
    let provision = IdpProvisionServiceAccountRequest::new(
        tenant_context(),
        "ci".to_owned(),
        vec!["platform.read".to_owned()],
    );
    assert_eq!(provision.tenant_context.tenant_id, Uuid::from_u128(1));
    assert_eq!(provision.name, "ci");

    let rotate = IdpRotateServiceAccountSecretRequest::new(tenant_context(), "svc-abc".to_owned());
    assert_eq!(rotate.client_id, "svc-abc");

    let revoke = IdpRevokeServiceAccountRequest::new(tenant_context(), "svc-abc".to_owned());
    assert_eq!(revoke.client_id, "svc-abc");

    let list = IdpListServiceAccountsRequest::new(tenant_context());
    assert_eq!(list.tenant_context.tenant_name, "acme");
}

/// The listing entry round-trips through Serde: out-of-process adapters
/// exchange it on the wire, and it carries no secret to leak there.
#[test]
fn summary_round_trips_through_serde() {
    let summary = IdpServiceAccountSummary::new(
        "svc-abc".to_owned(),
        "ci".to_owned(),
        true,
        vec!["platform.read".to_owned()],
    );
    let json = serde_json::to_string(&summary).expect("summary serialises");
    let back: IdpServiceAccountSummary = serde_json::from_str(&json).expect("summary parses");
    assert_eq!(back.client_id, "svc-abc");
    assert_eq!(back.name, "ci");
    assert!(back.enabled);
    assert_eq!(back.scopes, vec!["platform.read".to_owned()]);
}
