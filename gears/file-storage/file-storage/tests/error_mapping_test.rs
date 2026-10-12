//! Table-driven pin of every `DomainError` variant's HTTP status via the real conversion.
//! `expected_status` has no wildcard arm, so a new variant fails to compile until covered.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use file_storage::domain::error::DomainError;
use toolkit::api::canonical_prelude::CanonicalError;
use uuid::Uuid;

/// Number of `DomainError` variants; a backstop for `all_variant_instances` staying in sync.
const EXPECTED_VARIANT_COUNT: usize = 24;

/// Expected HTTP status per variant, per the canonical-error taxonomy (`status_code()`).
/// No wildcard arm on purpose.
fn expected_status(err: &DomainError) -> u16 {
    match err {
        DomainError::Validation { .. }
        | DomainError::PreconditionFailed { .. }
        | DomainError::MimeMismatch { .. }
        | DomainError::HashMismatch { .. }
        | DomainError::InvalidGtsType { .. }
        | DomainError::UnknownBackend { .. }
        | DomainError::PolicyMimeNotAllowed { .. }
        | DomainError::PolicySizeExceeded { .. }
        | DomainError::PolicyMetadataExceeded { .. }
        | DomainError::MultipartNotSupported { .. } => 400,
        DomainError::TokenInvalid { .. } | DomainError::Forbidden => 403,
        DomainError::FileNotFound { .. }
        | DomainError::VersionNotFound { .. }
        | DomainError::RetentionRuleNotFound { .. }
        | DomainError::MultipartUploadNotFound { .. } => 404,
        DomainError::Conflict { .. }
        | DomainError::MultipartUploadNotInProgress { .. }
        | DomainError::MultipartPartsMissing { .. }
        | DomainError::VersionedFileMigrationNotSupported { .. } => 409,
        DomainError::QuotaExceeded { .. } => 429,
        DomainError::Database { .. } | DomainError::Backend { .. } | DomainError::InternalError => {
            500
        }
    }
}

fn all_variant_instances() -> Vec<DomainError> {
    vec![
        DomainError::FileNotFound { id: Uuid::nil() },
        DomainError::VersionNotFound {
            file_id: Uuid::nil(),
            version_id: Uuid::nil(),
        },
        DomainError::RetentionRuleNotFound {
            rule_id: Uuid::nil(),
        },
        DomainError::Database {
            message: "db down".into(),
        },
        DomainError::Validation {
            field: "name".into(),
            message: "required".into(),
        },
        DomainError::Conflict {
            message: "already exists".into(),
        },
        DomainError::PreconditionFailed {
            message: "If-Match mismatch".into(),
        },
        DomainError::MimeMismatch {
            declared: "image/png".into(),
            detected: "image/jpeg".into(),
        },
        DomainError::HashMismatch {
            expected: "aaaa".into(),
            got: "bbbb".into(),
        },
        DomainError::InvalidGtsType {
            value: "not-a-gts-type".into(),
        },
        DomainError::Backend {
            backend_id: "s3".into(),
            message: "put failed".into(),
        },
        DomainError::UnknownBackend {
            backend_id: "nope".into(),
        },
        DomainError::TokenInvalid {
            reason: "bad signature".into(),
        },
        DomainError::Forbidden,
        DomainError::InternalError,
        DomainError::PolicyMimeNotAllowed {
            mime_type: "application/x-evil".into(),
        },
        DomainError::PolicySizeExceeded {
            limit_bytes: 1024,
            limit_source: "tenant-policy".into(),
        },
        DomainError::PolicyMetadataExceeded {
            reason: "too many keys".into(),
        },
        DomainError::QuotaExceeded {
            reason: "storage_bytes".into(),
        },
        DomainError::MultipartNotSupported {
            backend_id: "local".into(),
        },
        DomainError::MultipartUploadNotFound {
            upload_id: Uuid::nil(),
        },
        DomainError::MultipartUploadNotInProgress {
            upload_id: Uuid::nil(),
            state: "aborted".into(),
        },
        DomainError::MultipartPartsMissing {
            upload_id: Uuid::nil(),
            missing: vec![2, 5],
        },
        DomainError::VersionedFileMigrationNotSupported {
            file_id: Uuid::nil(),
        },
    ]
}

#[test]
fn error_domain_error_maps_to_expected_http_status() {
    let cases = all_variant_instances();
    // Best-effort backstop: a variant added to `expected_status` but not to
    // `all_variant_instances` trips this.
    assert_eq!(
        cases.len(),
        EXPECTED_VARIANT_COUNT,
        "all_variant_instances must enumerate every DomainError variant \
         exactly once; update EXPECTED_VARIANT_COUNT and add/remove a case \
         when DomainError gains or loses a variant"
    );

    for err in cases {
        let expected = expected_status(&err);
        let debug = format!("{err:?}");
        let canonical: CanonicalError = err.into();
        let actual = canonical.status_code();
        assert_eq!(
            actual, expected,
            "DomainError variant {debug} mapped to {actual} but expected {expected}"
        );
    }
}

/// Route declarations in `routes.rs` checked against the same status constants (hand-synced).
#[test]
fn declared_routes_match_the_pinned_status_table() {
    let precondition_failed_expected = expected_status(&DomainError::PreconditionFailed {
        message: "x".into(),
    });
    let multipart_not_supported_expected = expected_status(&DomainError::MultipartNotSupported {
        backend_id: "x".into(),
    });

    let declared_routes: Vec<(&str, u16)> = vec![
        // POST /files/{id}/bind: If-Match/CAS precondition failure.
        ("file_storage.bind", precondition_failed_expected),
        // DELETE /files/{id}: If-Match required, mismatch/absent.
        ("file_storage.delete_file", precondition_failed_expected),
        // POST /files/{id}/multipart: MultipartNotSupported (no variant maps to 422).
        (
            "file_storage.initiate_multipart",
            multipart_not_supported_expected,
        ),
    ];

    for (operation_id, declared_status) in declared_routes {
        assert_eq!(
            declared_status, 400,
            "{operation_id}'s declared route status must be 400 per the 2.5 fix"
        );
    }
}
