use super::*;
use crate::domain::error::UnsupportedResource;
use uuid::Uuid;

/// Stable test tenant id used by every conversion test below. Its
/// concrete value is irrelevant to the redaction / variant-mapping
/// invariants under test — only the `tenant_id` field on the
/// emitted `am.idp` log is shaped by it, and these tests don't
/// assert log payload, just the resulting `DomainError` shape.
fn fixture_tenant_id() -> Uuid {
    Uuid::from_u128(0xA11CE)
}

#[test]
fn clean_failure_maps_to_service_unavailable() {
    let err = IdpProvisionFailure::CleanFailure {
        detail: "conn refused".into(),
    }
    .into_domain_error(fixture_tenant_id());
    assert!(matches!(err, DomainError::ServiceUnavailable { .. }));
}

#[test]
fn provision_unsupported_operation_maps_to_unsupported_operation() {
    let err = IdpProvisionFailure::UnsupportedOperation {
        detail: "not supported by provider".into(),
    }
    .into_domain_error(fixture_tenant_id());
    let DomainError::UnsupportedOperation { detail, resource } = err else {
        panic!("expected UnsupportedOperation");
    };
    // The tenant half must tag itself as such: the boundary picks the
    // envelope's `resource_type` from this, and all three halves collapse
    // into the same variant.
    assert_eq!(resource, UnsupportedResource::Tenant);
    // Public detail MUST carry the redaction marker and MUST NOT
    // leak the raw provider string.
    assert!(
        detail.contains("detail redacted"),
        "missing redaction marker in public detail: {detail}"
    );
    assert!(
        !detail.contains("not supported by provider"),
        "raw provider string leaked into public detail: {detail}"
    );
}

#[test]
fn provision_ambiguous_maps_to_internal_with_redacted_diagnostic() {
    let err = IdpProvisionFailure::Ambiguous {
        detail: "vendor stack trace with token=secret-LEAK-9f3a7c2e".into(),
    }
    .into_domain_error(fixture_tenant_id());
    let DomainError::Internal { diagnostic, .. } = err else {
        panic!("expected Internal");
    };
    assert!(diagnostic.contains("provider detail redacted"));
    // Pin the redaction contract for vendor-text leaks: even a
    // sentinel-shaped token in `detail` MUST NOT reach the public
    // `Internal::diagnostic` field (which is forwarded verbatim into
    // `Problem.detail` by the canonical-mapping boundary). The
    // symmetric Deprovision-side coverage previously lived in this
    // file too; with `DeprovisionFailureExt` removed (no production
    // callers — see `domain::idp::mod`), the redaction-helper itself
    // is exercised by the Provision tests since both Provision and
    // Deprovision conversions share `redact_provider_detail`.
    assert!(
        !diagnostic.contains("token="),
        "raw vendor token leaked into Internal diagnostic: {diagnostic}"
    );
    assert!(
        !diagnostic.contains("secret-LEAK-9f3a7c2e"),
        "raw vendor sentinel leaked into Internal diagnostic: {diagnostic}"
    );
}

/// Pins the cause-chain contract on the Ambiguous → Internal
/// mapping: `Error::source` MUST surface a typed wrapper so
/// downstream retry-classification / observability consumers can
/// walk the chain, while `Display` on the wrapper MUST NOT carry
/// raw vendor text (only the redacted digest + length already
/// emitted on the `am.idp` log line). Closes deep-review #5.
#[test]
fn provision_ambiguous_chains_redacted_cause_without_leaking_raw_detail() {
    use std::error::Error as _;

    let raw_detail = "vendor stack trace with token=secret-LEAK-c0ffee99";
    let err = IdpProvisionFailure::Ambiguous {
        detail: raw_detail.to_owned(),
    }
    .into_domain_error(fixture_tenant_id());
    let DomainError::Internal { cause, .. } = &err else {
        panic!("expected Internal");
    };
    let cause = cause
        .as_ref()
        .expect("Ambiguous mapping MUST chain a typed cause for retry-classification");
    let source_chain = cause.to_string();
    assert!(
        source_chain.contains("digest=") && source_chain.contains("len="),
        "redacted cause MUST carry digest+len for operator correlation; got: {source_chain}"
    );
    assert!(
        !source_chain.contains("token=") && !source_chain.contains("secret-LEAK-c0ffee99"),
        "raw vendor text leaked into redacted cause Display: {source_chain}"
    );
    // Walkable: `Error::source` returns None at the wrapper (it is
    // the leaf), but the wrapper itself MUST be reachable via
    // `Error::source` on the DomainError envelope (`#[source]`
    // attribute on the `cause` field).
    assert!(
        err.source().is_some(),
        "DomainError::Internal::cause MUST be reachable via Error::source"
    );
}

// ---------------------------------------------------------------------------
// Classified user-operation rejections.
// ---------------------------------------------------------------------------

#[test]
fn user_duplicate_username_maps_to_user_already_exists() {
    let err = IdpUserOperationFailure::DuplicateUser {
        field: account_management_sdk::IdpUserDuplicateField::Username,
        detail: "User exists with same username".into(),
    }
    .into_domain_error(fixture_tenant_id());
    // The typed field survives verbatim; the curated public wording is
    // derived (once) at the canonical boundary, so raw provider text
    // cannot leak from here by construction.
    assert!(
        matches!(
            err,
            DomainError::UserAlreadyExists {
                field: account_management_sdk::IdpUserDuplicateField::Username
            }
        ),
        "expected UserAlreadyExists(Username), got {err:?}"
    );
}

#[test]
fn user_duplicate_email_maps_to_user_already_exists() {
    let err = IdpUserOperationFailure::DuplicateUser {
        field: account_management_sdk::IdpUserDuplicateField::Email,
        detail: "User exists with same email".into(),
    }
    .into_domain_error(fixture_tenant_id());
    assert!(
        matches!(
            err,
            DomainError::UserAlreadyExists {
                field: account_management_sdk::IdpUserDuplicateField::Email
            }
        ),
        "expected UserAlreadyExists(Email), got {err:?}"
    );
}

// KC's ModelDuplicateException path emits the combined constant "User
// exists with same username or email" (on KC 26 it is the only 409
// `createUser` produces directly) — the unattributable classification
// must survive to the domain error, not collapse into Username.
#[test]
fn user_duplicate_combined_maps_to_username_or_email() {
    let err = IdpUserOperationFailure::DuplicateUser {
        field: account_management_sdk::IdpUserDuplicateField::UsernameOrEmail,
        detail: "User exists with same username or email".into(),
    }
    .into_domain_error(fixture_tenant_id());
    assert!(
        matches!(
            err,
            DomainError::UserAlreadyExists {
                field: account_management_sdk::IdpUserDuplicateField::UsernameOrEmail
            }
        ),
        "expected UserAlreadyExists(UsernameOrEmail), got {err:?}"
    );
}

#[test]
fn user_password_policy_maps_to_structured_password_violation() {
    let err = IdpUserOperationFailure::PasswordPolicy {
        detail: "invalidPasswordMinLengthMessage: 12".into(),
    }
    .into_domain_error(fixture_tenant_id());
    let DomainError::IdpPasswordPolicy { detail } = err else {
        panic!("expected IdpPasswordPolicy, got a different variant");
    };
    // Curated public summary only — the raw KC policy text stays in
    // the digest-only log line.
    assert!(
        !detail.contains("invalidPasswordMinLengthMessage"),
        "raw provider policy text leaked into public detail: {detail}"
    );
}

// The unclassified catch-all keeps its historical shape: a provider
// rejection the plugin could not attribute still collapses to the
// redacted generic Validation (no accidental behavior change for
// legacy plugins that only emit `Rejected`).
#[test]
fn user_rejected_still_maps_to_redacted_validation() {
    let err = IdpUserOperationFailure::Rejected {
        detail: "something vendor-specific".into(),
    }
    .into_domain_error(fixture_tenant_id());
    let DomainError::Validation { detail } = err else {
        panic!("expected Validation, got a different variant");
    };
    assert!(detail.contains("detail redacted"), "got: {detail}");
}
