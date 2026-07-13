//! Tests for the [`AccountManagementError`] projection.
//!
//! Two suites:
//!
//! * `wire_vocabulary_round_trip` — pins every wire-string constant the
//!   projection introduces ([`crate::field`], [`crate::precondition`],
//!   [`crate::reason`], [`crate::quota`], [`crate::gts`]) to its
//!   `Problem` JSON path. A drift between an SDK constant and the wire
//!   trips here.
//! * `projection_tests` — exercises `From<CanonicalError>`, verifying
//!   each canonical category lands on the expected typed variant and
//!   that unmodeled categories preserve the canonical in `Other`.

use super::AccountManagementError;

// ─────────────────────────────────────────────────────────────────────
// Wire-vocabulary round-trip
// ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod wire_vocabulary_round_trip {
    use crate::{field, gts, precondition, quota, reason};
    use toolkit_canonical_errors::{CanonicalError, Problem, resource_error};

    // Test scopes mirroring the impl crate's `#[resource_error]` markers.
    // Their literals MUST equal the `gts::*` constants — the
    // `gts_resource_types_round_trip` test asserts that equality.
    #[resource_error(gts_id!("cf.core.am.tenant.v1~"))]
    struct TenantScope;

    #[resource_error(gts_id!("cf.core.am.user.v1~"))]
    struct UserScope;

    #[resource_error(gts_id!("cf.core.am.tenant_metadata.v1~"))]
    struct MetadataScope;

    #[resource_error(gts_id!("cf.core.am.conversion_request.v1~"))]
    struct ConversionScope;

    fn problem(err: CanonicalError) -> serde_json::Value {
        serde_json::to_value(Problem::from(err)).expect("Problem serializes")
    }

    #[test]
    fn gts_resource_types_round_trip_to_context_resource_type() {
        let cases = [
            (
                TenantScope::not_found("x").with_resource("x").create(),
                gts::TENANT_RESOURCE_TYPE,
            ),
            (
                UserScope::not_found("x").with_resource("x").create(),
                gts::USER_RESOURCE_TYPE,
            ),
            (
                MetadataScope::not_found("x").with_resource("x").create(),
                gts::TENANT_METADATA_RESOURCE_TYPE,
            ),
            (
                ConversionScope::not_found("x").with_resource("x").create(),
                gts::CONVERSION_REQUEST_RESOURCE_TYPE,
            ),
        ];
        for (err, expected) in cases {
            let json = problem(err);
            assert_eq!(
                json["context"]["resource_type"], expected,
                "resource type {expected} must round-trip into context.resource_type",
            );
        }
    }

    #[test]
    fn field_reason_constants_round_trip_to_field_violations() {
        for code in [
            field::INVALID_TENANT_TYPE,
            field::VALIDATION,
            field::ROOT_TENANT_CANNOT_DELETE,
            field::ROOT_TENANT_CANNOT_CONVERT,
            field::ROOT_TENANT_CANNOT_CHANGE_STATUS,
            field::IDP_INVALID_INPUT,
        ] {
            let err = TenantScope::invalid_argument()
                .with_field_violation("test_field", "test description", code)
                .create();
            let json = problem(err);
            assert_eq!(
                json["context"]["field_violations"][0]["reason"], code,
                "field reason {code} must round-trip into field_violations[].reason",
            );
        }
    }

    #[test]
    fn field_name_constants_round_trip_to_field_violations() {
        for name in [
            field::TENANT_TYPE_FIELD,
            field::REQUEST_FIELD,
            field::METADATA_FIELD,
            field::TENANT_ID_FIELD,
            field::PROVISIONING_METADATA_FIELD,
        ] {
            let err = TenantScope::invalid_argument()
                .with_field_violation(name, "test description", field::VALIDATION)
                .create();
            let json = problem(err);
            assert_eq!(
                json["context"]["field_violations"][0]["field"], name,
                "field name {name} must round-trip into field_violations[].field",
            );
        }
    }

    #[test]
    fn precondition_subject_constants_round_trip_to_violations() {
        for subject in [
            precondition::TENANT_TYPE_SUBJECT,
            precondition::DEPTH_SUBJECT,
            precondition::TENANT_SUBJECT,
            precondition::REQUEST_SUBJECT,
            precondition::CONFIGURATION_SUBJECT,
            precondition::CONVERSION_REQUEST_SUBJECT,
        ] {
            let err = TenantScope::failed_precondition()
                .with_precondition_violation(subject, "test description", "TEST_TYPE")
                .create();
            let json = problem(err);
            assert_eq!(
                json["context"]["violations"][0]["subject"], subject,
                "subject {subject} must round-trip into violations[].subject",
            );
        }
    }

    #[test]
    fn precondition_type_constants_round_trip_to_violations() {
        for type_ in [
            precondition::TYPE_NOT_ALLOWED_TYPE,
            precondition::TENANT_DEPTH_EXCEEDED_TYPE,
            precondition::TENANT_HAS_CHILDREN_TYPE,
            precondition::TENANT_HAS_RESOURCES_TYPE,
            precondition::PRECONDITION_FAILED_TYPE,
            precondition::FEATURE_DISABLED_TYPE,
            precondition::INVALID_ACTOR_FOR_TRANSITION_TYPE,
            precondition::ALREADY_RESOLVED_TYPE,
        ] {
            let err = TenantScope::failed_precondition()
                .with_precondition_violation("test_subject", "test description", type_)
                .create();
            let json = problem(err);
            assert_eq!(
                json["context"]["violations"][0]["type"], type_,
                "type {type_} must round-trip into violations[].type",
            );
        }
    }

    #[test]
    fn aborted_reason_constants_round_trip_to_context_reason() {
        for r in [
            reason::aborted::METADATA_VERSION_MISMATCH,
            reason::aborted::SERIALIZATION_CONFLICT,
        ] {
            let err = TenantScope::aborted("conflict").with_reason(r).create();
            let json = problem(err);
            assert_eq!(
                json["context"]["reason"], r,
                "aborted reason {r} must round-trip into context.reason",
            );
        }
    }

    #[test]
    fn permission_reason_constant_round_trips_to_context_reason() {
        let r = reason::permission::CROSS_TENANT_DENIED;
        let err = TenantScope::permission_denied().with_reason(r).create();
        let json = problem(err);
        assert_eq!(json["context"]["reason"], r);
    }

    #[test]
    fn quota_subject_constant_round_trips_to_violations() {
        let err = TenantScope::resource_exhausted("integrity check already in progress")
            .with_quota_violation(quota::INTEGRITY_CHECK, "another check is in progress")
            .create();
        let json = problem(err);
        assert_eq!(
            json["context"]["violations"][0]["subject"],
            quota::INTEGRITY_CHECK,
        );
    }
}

// ─────────────────────────────────────────────────────────────────────
// Projection: From<CanonicalError> for AccountManagementError
// ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod projection_tests {
    use super::AccountManagementError;
    use crate::field::ValidationReason;
    use crate::precondition::Subject;
    use crate::{field, gts, precondition, quota, reason};
    use toolkit_canonical_errors::{CanonicalError, Problem, resource_error};

    #[resource_error(gts_id!("cf.core.am.tenant.v1~"))]
    struct TenantScope;

    #[resource_error(gts_id!("cf.core.am.tenant_metadata.v1~"))]
    struct MetadataScope;

    #[test]
    fn not_found_projects_resource_type_and_name() {
        let canonical = TenantScope::not_found("tenant 7 not found")
            .with_resource("7")
            .create();
        match AccountManagementError::from(canonical) {
            AccountManagementError::NotFound {
                resource_type,
                name,
                ..
            } => {
                assert_eq!(resource_type, gts::TENANT_RESOURCE_TYPE);
                assert_eq!(name, "7");
            }
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn already_exists_projects_resource_type_and_name() {
        let canonical = TenantScope::already_exists("tenant exists")
            .with_resource("tenant")
            .create();
        match AccountManagementError::from(canonical) {
            AccountManagementError::AlreadyExists {
                resource_type,
                name,
                ..
            } => {
                assert_eq!(resource_type, gts::TENANT_RESOURCE_TYPE);
                assert_eq!(name, "tenant");
            }
            other => panic!("expected AlreadyExists, got {other:?}"),
        }
    }

    #[test]
    fn invalid_argument_projects_typed_reason() {
        let cases = [
            (
                field::INVALID_TENANT_TYPE,
                ValidationReason::InvalidTenantType,
            ),
            (field::VALIDATION, ValidationReason::Validation),
            (
                field::ROOT_TENANT_CANNOT_DELETE,
                ValidationReason::RootTenantCannotDelete,
            ),
            (field::IDP_INVALID_INPUT, ValidationReason::IdpInvalidInput),
        ];
        for (wire, expected) in cases {
            let canonical = TenantScope::invalid_argument()
                .with_field_violation(field::TENANT_TYPE_FIELD, "bad", wire)
                .create();
            match AccountManagementError::from(canonical) {
                AccountManagementError::InvalidArgument {
                    field,
                    reason,
                    detail,
                } => {
                    assert_eq!(field, field::TENANT_TYPE_FIELD);
                    assert_eq!(reason, expected);
                    assert_eq!(detail, "bad");
                }
                other => panic!("expected InvalidArgument for {wire}, got {other:?}"),
            }
        }
    }

    #[test]
    fn invalid_argument_unknown_reason_preserved() {
        let canonical = TenantScope::invalid_argument()
            .with_field_violation("f", "d", "FUTURE_CODE")
            .create();
        match AccountManagementError::from(canonical) {
            AccountManagementError::InvalidArgument { reason, .. } => {
                assert_eq!(reason, ValidationReason::Unknown("FUTURE_CODE".to_owned()));
            }
            other => panic!("expected InvalidArgument::Unknown, got {other:?}"),
        }
    }

    #[test]
    fn invalid_argument_format_variant_projects_empty_field() {
        let canonical = TenantScope::invalid_argument()
            .with_format("alias must be 1-63 chars")
            .create();
        match AccountManagementError::from(canonical) {
            AccountManagementError::InvalidArgument { field, reason, .. } => {
                assert!(field.is_empty());
                assert_eq!(reason, ValidationReason::Unknown(String::new()));
            }
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }

    #[test]
    fn failed_precondition_projects_typed_subject() {
        let cases = [
            (precondition::TENANT_TYPE_SUBJECT, Subject::TenantType),
            (precondition::DEPTH_SUBJECT, Subject::Depth),
            (precondition::TENANT_SUBJECT, Subject::Tenant),
            (
                precondition::CONVERSION_REQUEST_SUBJECT,
                Subject::ConversionRequest,
            ),
        ];
        for (wire, expected) in cases {
            let canonical = TenantScope::failed_precondition()
                .with_precondition_violation(
                    wire,
                    "guard message",
                    precondition::TENANT_HAS_CHILDREN_TYPE,
                )
                .create();
            match AccountManagementError::from(canonical) {
                AccountManagementError::FailedPrecondition {
                    subject,
                    type_,
                    detail,
                } => {
                    assert_eq!(subject, expected);
                    assert_eq!(type_, precondition::TENANT_HAS_CHILDREN_TYPE);
                    assert_eq!(detail, "guard message");
                }
                other => panic!("expected FailedPrecondition for {wire}, got {other:?}"),
            }
        }
    }

    #[test]
    fn aborted_projects_reason() {
        let canonical = MetadataScope::aborted("version mismatch")
            .with_reason(reason::aborted::METADATA_VERSION_MISMATCH)
            .create();
        match AccountManagementError::from(canonical) {
            AccountManagementError::Aborted { reason, detail } => {
                assert_eq!(reason, reason::aborted::METADATA_VERSION_MISMATCH);
                assert_eq!(detail, "version mismatch");
            }
            other => panic!("expected Aborted, got {other:?}"),
        }
    }

    #[test]
    fn permission_denied_projects_reason() {
        let canonical = TenantScope::permission_denied()
            .with_reason(reason::permission::CROSS_TENANT_DENIED)
            .create();
        match AccountManagementError::from(canonical) {
            AccountManagementError::PermissionDenied { reason, .. } => {
                assert_eq!(reason, reason::permission::CROSS_TENANT_DENIED);
            }
            other => panic!("expected PermissionDenied, got {other:?}"),
        }
    }

    #[test]
    fn unimplemented_projects() {
        let canonical = TenantScope::unimplemented("not supported").create();
        assert!(matches!(
            AccountManagementError::from(canonical),
            AccountManagementError::Unimplemented { .. }
        ));
    }

    #[test]
    fn service_unavailable_projects_with_retry_after() {
        let canonical = CanonicalError::service_unavailable()
            .with_retry_after_seconds(30)
            .create();
        match AccountManagementError::from(canonical) {
            AccountManagementError::Unavailable {
                retry_after_seconds: Some(30),
                ..
            } => {}
            other => panic!("expected Unavailable with retry=30, got {other:?}"),
        }
    }

    #[test]
    fn resource_exhausted_projects_subject() {
        let canonical = TenantScope::resource_exhausted("integrity check in progress")
            .with_quota_violation(quota::INTEGRITY_CHECK, "another check is in progress")
            .create();
        match AccountManagementError::from(canonical) {
            AccountManagementError::ResourceExhausted { subject, .. } => {
                assert_eq!(subject, quota::INTEGRITY_CHECK);
            }
            other => panic!("expected ResourceExhausted, got {other:?}"),
        }
    }

    #[test]
    fn internal_projects() {
        let canonical = CanonicalError::internal("boom").create();
        assert!(matches!(
            AccountManagementError::from(canonical),
            AccountManagementError::Internal { .. }
        ));
    }

    #[test]
    fn unmodeled_category_falls_through_to_other() {
        // AM never emits Unauthenticated; it must land in Other with the
        // canonical preserved for inspection.
        let canonical = CanonicalError::unauthenticated()
            .with_reason("SOME_REASON")
            .create();
        match AccountManagementError::from(canonical) {
            AccountManagementError::Other {
                canonical: CanonicalError::Unauthenticated { .. },
            } => {}
            other => panic!("expected Other::Unauthenticated, got {other:?}"),
        }
    }

    #[test]
    fn projection_survives_full_problem_round_trip() {
        // Out-of-process chain: canonical → Problem JSON → Problem →
        // CanonicalError → AccountManagementError. Pins that an HTTP
        // consumer projecting from the wire gets the same typed variant
        // as an in-process ClientHub caller.
        let canonical = TenantScope::failed_precondition()
            .with_precondition_violation(
                precondition::TENANT_SUBJECT,
                "tenant has child tenants",
                precondition::TENANT_HAS_CHILDREN_TYPE,
            )
            .create();

        let bytes = serde_json::to_vec(&Problem::from(canonical)).expect("serialize");
        let restored: Problem = serde_json::from_slice(&bytes).expect("deserialize");
        let restored_canonical = CanonicalError::try_from(restored).expect("reconstruct");

        match AccountManagementError::from(restored_canonical) {
            AccountManagementError::FailedPrecondition { subject, type_, .. } => {
                assert_eq!(subject, Subject::Tenant);
                assert_eq!(type_, precondition::TENANT_HAS_CHILDREN_TYPE);
            }
            other => panic!("expected FailedPrecondition after round-trip, got {other:?}"),
        }
    }
}
