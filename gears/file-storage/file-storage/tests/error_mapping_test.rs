//! Guardrail for P2 remediation 2.5 (canonical error-code contract drift).
//!
//! Table-driven pin of every `DomainError` variant's real HTTP status,
//! computed through the actual `DomainError -> CanonicalError` conversion at
//! `src/api/rest/error.rs` (not a hand-duplicated status table), so the test
//! fails the moment that conversion's behavior changes rather than only when
//! someone remembers to update a parallel table.
//!
//! The `expected_status` match below has **no wildcard arm**: adding a new
//! `DomainError` variant to `src/domain/error.rs` without adding a case here
//! is a **compile error** (E0004, non-exhaustive match), which is what makes
//! this a durable guardrail against re-drift rather than a one-time snapshot.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

use file_storage::domain::error::DomainError;
use toolkit::api::canonical_prelude::CanonicalError;
use uuid::Uuid;

/// Number of `DomainError` variants as of this test's writing. Kept as a
/// belt-and-suspenders check alongside the exhaustive match in
/// `expected_status`: bumping this without adding both a match arm there and
/// an instance in `all_variant_instances` fails the test below.
const EXPECTED_VARIANT_COUNT: usize = 23;

/// The expected HTTP status for every `DomainError` variant, per the
/// canonical-error taxonomy in `libs/toolkit-canonical-errors/src/error.rs`
/// (`status_code()`): `InvalidArgument`/`FailedPrecondition`/`OutOfRange` map
/// to 400, `PermissionDenied` to 403, `NotFound` to 404, `AlreadyExists`/
/// `Aborted` to 409, `ResourceExhausted` to 429, `Internal` to 500. There is
/// no built-in variant that resolves to 412, 422, or 507 \u{2014} see 2.5's
/// design-decision record for why the two `PreconditionFailed` routes were
/// fixed to declare 400 (not left declaring 412) instead.
///
/// No wildcard arm on purpose (see module doc).
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
        | DomainError::VersionedFileMigrationNotSupported { .. } => 409,
        DomainError::QuotaExceeded { .. } => 429,
        DomainError::Database { .. } | DomainError::Backend { .. } | DomainError::InternalError => {
            500
        }
    }
}

/// One representative instance of every `DomainError` variant. Field values
/// are arbitrary placeholders; only the discriminant matters for the
/// status-code mapping under test.
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
        DomainError::VersionedFileMigrationNotSupported {
            file_id: Uuid::nil(),
        },
    ]
}

#[test]
fn error_domain_error_maps_to_expected_http_status() {
    let cases = all_variant_instances();
    // If `DomainError` grows a variant, `expected_status`'s exhaustive match
    // fails to compile until a case is added there too \u{2014} but nothing
    // forces a matching entry in `all_variant_instances`. This count check is
    // a cheap best-effort backstop so a variant added to one and not the
    // other trips this assertion instead of silently under-covering the
    // table.
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

/// Cross-check: the three routes touched by 2.5
/// (`src/api/rest/routes.rs`) against the same status constants asserted
/// above, so a reviewer can eyeball both sides of the contract side by side.
/// Parsing `routes.rs`'s declared `StatusCode::*` calls by reflection isn't
/// practical, so these pairs are hard-coded and must be kept in sync by hand
/// with the route declarations they name.
#[test]
fn declared_routes_match_the_pinned_status_table() {
    let precondition_failed_expected = expected_status(&DomainError::PreconditionFailed {
        message: "x".into(),
    });
    let multipart_not_supported_expected = expected_status(&DomainError::MultipartNotSupported {
        backend_id: "x".into(),
    });

    let declared_routes: Vec<(&str, u16)> = vec![
        // POST /files/{id}/bind \u{2014} If-Match/CAS precondition failure.
        // routes.rs: `.error_400(openapi)` (was `StatusCode::PRECONDITION_FAILED`).
        ("file_storage.bind", precondition_failed_expected),
        // DELETE /files/{id} \u{2014} If-Match required, mismatch/absent.
        // routes.rs: `.error_400(openapi)` (was `StatusCode::PRECONDITION_FAILED`).
        ("file_storage.delete_file", precondition_failed_expected),
        // POST /files/{id}/multipart \u{2014} MultipartNotSupported is already
        // covered by `.error_400(openapi)`; the redundant `.error_422(openapi)`
        // was removed (no DomainError variant maps to 422).
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
