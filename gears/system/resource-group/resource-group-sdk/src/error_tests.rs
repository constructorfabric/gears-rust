//! Tests for the [`ResourceGroupError`] projection.
//!
//! Two suites:
//!
//! * `wire_vocabulary_round_trip` — pins every wire-string constant the
//!   projection introduces ([`crate::field`], [`crate::precondition`],
//!   [`crate::reason`], [`crate::gts`]) to its `Problem` JSON path. A
//!   drift between an SDK constant and the wire trips here.
//! * `projection_tests` — exercises `From<CanonicalError>`, verifying
//!   each canonical category lands on the expected typed variant and that
//!   unmodeled categories preserve the canonical in `Other`.

use super::ResourceGroupError;

// ─────────────────────────────────────────────────────────────────────
// Wire-vocabulary round-trip
// ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod wire_vocabulary_round_trip {
    use crate::{field, gts, precondition, reason};
    use toolkit_canonical_errors::{CanonicalError, Problem, resource_error};

    // Test scope mirroring the impl crate's `#[resource_error]` marker.
    // Its literal MUST equal `gts::GROUP_RESOURCE_TYPE` — the
    // `gts_resource_type_round_trip` test asserts that equality (the
    // proc-macro cannot reference the const directly).
    #[resource_error(gts_id!("cf.core.resource_group.group.v1~"))]
    struct RgScope;

    fn problem(err: CanonicalError) -> serde_json::Value {
        serde_json::to_value(Problem::from(err)).expect("Problem serializes")
    }

    #[test]
    fn gts_resource_type_round_trips_to_context_resource_type() {
        let err = RgScope::not_found("x").with_resource("x").create();
        let json = problem(err);
        assert_eq!(
            json["context"]["resource_type"],
            gts::GROUP_RESOURCE_TYPE,
            "resource type must round-trip into context.resource_type",
        );
    }

    #[test]
    fn field_reason_constant_round_trips_to_field_violations() {
        let err = RgScope::invalid_argument()
            .with_field_violation(
                field::PARENT_TYPE_FIELD,
                "bad parent",
                field::INVALID_PARENT_TYPE,
            )
            .create();
        let json = problem(err);
        assert_eq!(
            json["context"]["field_violations"][0]["reason"],
            field::INVALID_PARENT_TYPE,
            "field reason must round-trip into field_violations[].reason",
        );
        assert_eq!(
            json["context"]["field_violations"][0]["field"],
            field::PARENT_TYPE_FIELD,
            "field name must round-trip into field_violations[].field",
        );
    }

    #[test]
    fn precondition_subject_constants_round_trip_to_violations() {
        for subject in [
            precondition::ALLOWED_PARENTS_SUBJECT,
            precondition::HIERARCHY_SUBJECT,
            precondition::ACTIVE_REFERENCES_SUBJECT,
            precondition::LIMIT_SUBJECT,
            precondition::TENANT_SUBJECT,
        ] {
            let err = RgScope::failed_precondition()
                .with_precondition_violation(subject, "test description", precondition::STATE_TYPE)
                .create();
            let json = problem(err);
            assert_eq!(
                json["context"]["violations"][0]["subject"], subject,
                "subject {subject} must round-trip into violations[].subject",
            );
        }
    }

    #[test]
    fn precondition_type_constant_round_trips_to_violations() {
        let err = RgScope::failed_precondition()
            .with_precondition_violation(
                precondition::HIERARCHY_SUBJECT,
                "test description",
                precondition::STATE_TYPE,
            )
            .create();
        let json = problem(err);
        assert_eq!(
            json["context"]["violations"][0]["type"],
            precondition::STATE_TYPE,
            "type must round-trip into violations[].type",
        );
    }

    #[test]
    fn aborted_reason_constant_round_trips_to_context_reason() {
        let err = RgScope::aborted("conflict")
            .with_reason(reason::aborted::CONFLICT)
            .create();
        let json = problem(err);
        assert_eq!(json["context"]["reason"], reason::aborted::CONFLICT);
    }

    #[test]
    fn permission_reason_constant_round_trips_to_context_reason() {
        let err = RgScope::permission_denied()
            .with_reason(reason::permission::ACCESS_DENIED)
            .create();
        let json = problem(err);
        assert_eq!(json["context"]["reason"], reason::permission::ACCESS_DENIED);
    }
}

// ─────────────────────────────────────────────────────────────────────
// Projection: From<CanonicalError> for ResourceGroupError
// ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod projection_tests {
    use super::ResourceGroupError;
    use crate::precondition::Subject;
    use crate::{field, gts, precondition, reason};
    use toolkit_canonical_errors::{CanonicalError, Problem, resource_error};

    #[resource_error(gts_id!("cf.core.resource_group.group.v1~"))]
    struct RgScope;

    #[test]
    fn not_found_projects_resource_type_and_name() {
        let canonical = RgScope::not_found("group 7 not found")
            .with_resource("7")
            .create();
        match ResourceGroupError::from(canonical) {
            ResourceGroupError::NotFound {
                resource_type,
                name,
                ..
            } => {
                assert_eq!(resource_type, gts::GROUP_RESOURCE_TYPE);
                assert_eq!(name, "7");
            }
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn already_exists_projects_resource_type_and_name() {
        let canonical = RgScope::already_exists("type exists")
            .with_resource("my.code")
            .create();
        match ResourceGroupError::from(canonical) {
            ResourceGroupError::AlreadyExists {
                resource_type,
                name,
                ..
            } => {
                assert_eq!(resource_type, gts::GROUP_RESOURCE_TYPE);
                assert_eq!(name, "my.code");
            }
            other => panic!("expected AlreadyExists, got {other:?}"),
        }
    }

    #[test]
    fn invalid_argument_field_violation_projects_field_and_reason() {
        let canonical = RgScope::invalid_argument()
            .with_field_violation(
                field::PARENT_TYPE_FIELD,
                "bad parent",
                field::INVALID_PARENT_TYPE,
            )
            .create();
        match ResourceGroupError::from(canonical) {
            ResourceGroupError::InvalidArgument {
                field,
                reason,
                detail,
            } => {
                assert_eq!(field, field::PARENT_TYPE_FIELD);
                assert_eq!(reason, field::INVALID_PARENT_TYPE);
                assert_eq!(detail, "bad parent");
            }
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }

    #[test]
    fn invalid_argument_format_variant_projects_empty_field() {
        let canonical = RgScope::invalid_argument()
            .with_format("group name must be 1-63 chars")
            .create();
        match ResourceGroupError::from(canonical) {
            ResourceGroupError::InvalidArgument { field, reason, .. } => {
                assert!(field.is_empty());
                assert!(reason.is_empty());
            }
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }

    #[test]
    fn invalid_argument_constraint_variant_projects_constraint_into_reason() {
        let canonical = RgScope::invalid_argument()
            .with_constraint("parent_depth_limit")
            .create();
        match ResourceGroupError::from(canonical) {
            ResourceGroupError::InvalidArgument {
                field,
                reason,
                detail,
            } => {
                assert!(field.is_empty());
                assert_eq!(reason, "parent_depth_limit");
                assert!(!detail.is_empty());
            }
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }

    #[test]
    fn failed_precondition_projects_typed_subject() {
        let cases = [
            (
                precondition::ALLOWED_PARENTS_SUBJECT,
                Subject::AllowedParents,
            ),
            (precondition::HIERARCHY_SUBJECT, Subject::Hierarchy),
            (
                precondition::ACTIVE_REFERENCES_SUBJECT,
                Subject::ActiveReferences,
            ),
            (precondition::LIMIT_SUBJECT, Subject::Limit),
            (precondition::TENANT_SUBJECT, Subject::Tenant),
        ];
        for (wire, expected) in cases {
            let canonical = RgScope::failed_precondition()
                .with_precondition_violation(wire, "guard message", precondition::STATE_TYPE)
                .create();
            match ResourceGroupError::from(canonical) {
                ResourceGroupError::FailedPrecondition {
                    subject,
                    type_,
                    detail,
                } => {
                    assert_eq!(subject, expected);
                    assert_eq!(type_, precondition::STATE_TYPE);
                    assert_eq!(detail, "guard message");
                }
                other => panic!("expected FailedPrecondition for {wire}, got {other:?}"),
            }
        }
    }

    #[test]
    fn aborted_projects_reason() {
        let canonical = RgScope::aborted("write conflict")
            .with_reason(reason::aborted::CONFLICT)
            .create();
        match ResourceGroupError::from(canonical) {
            ResourceGroupError::Aborted { reason, detail } => {
                assert_eq!(reason, reason::aborted::CONFLICT);
                assert_eq!(detail, "write conflict");
            }
            other => panic!("expected Aborted, got {other:?}"),
        }
    }

    #[test]
    fn permission_denied_projects_reason() {
        let canonical = RgScope::permission_denied()
            .with_reason(reason::permission::ACCESS_DENIED)
            .create();
        match ResourceGroupError::from(canonical) {
            ResourceGroupError::PermissionDenied { reason, .. } => {
                assert_eq!(reason, reason::permission::ACCESS_DENIED);
            }
            other => panic!("expected PermissionDenied, got {other:?}"),
        }
    }

    #[test]
    fn internal_projects() {
        let canonical = CanonicalError::internal("boom").create();
        assert!(matches!(
            ResourceGroupError::from(canonical),
            ResourceGroupError::Internal { .. }
        ));
    }

    #[test]
    fn unmodeled_category_falls_through_to_other() {
        // Resource-group never emits Unauthenticated; it must land in
        // Other with the canonical preserved for inspection.
        let canonical = CanonicalError::unauthenticated()
            .with_reason("SOME_REASON")
            .create();
        match ResourceGroupError::from(canonical) {
            ResourceGroupError::Other {
                canonical: CanonicalError::Unauthenticated { .. },
            } => {}
            other => panic!("expected Other::Unauthenticated, got {other:?}"),
        }
    }

    /// Reconstruct a `CanonicalError` from a `Problem` JSON after mutating
    /// it, exercising the documented wire path the builder's type-state
    /// cannot reach in-process.
    fn canonical_from_mutated_problem(
        canonical: CanonicalError,
        mutate: impl FnOnce(&mut serde_json::Value),
    ) -> CanonicalError {
        let mut value = serde_json::to_value(Problem::from(canonical)).expect("Problem serializes");
        mutate(&mut value);
        let problem: Problem = serde_json::from_value(value).expect("Problem deserializes");
        CanonicalError::try_from(problem).expect("reconstruct")
    }

    #[test]
    fn not_found_without_resource_type_falls_through_to_other() {
        // A malformed / foreign NotFound envelope (no resource_type) must
        // NOT project to NotFound { resource_type: "" } — it would silently
        // mis-dispatch consumers matching on gts::GROUP_RESOURCE_TYPE.
        let canonical = RgScope::not_found("missing").with_resource("7").create();
        let malformed = canonical_from_mutated_problem(canonical, |v| {
            v["context"]
                .as_object_mut()
                .expect("context object")
                .remove("resource_type");
        });
        match ResourceGroupError::from(malformed) {
            ResourceGroupError::Other { .. } => {}
            other => panic!("expected Other for resource_type-less NotFound, got {other:?}"),
        }
    }

    #[test]
    fn already_exists_without_resource_type_falls_through_to_other() {
        let canonical = RgScope::already_exists("dupe")
            .with_resource("my.code")
            .create();
        let malformed = canonical_from_mutated_problem(canonical, |v| {
            v["context"]
                .as_object_mut()
                .expect("context object")
                .remove("resource_type");
        });
        match ResourceGroupError::from(malformed) {
            ResourceGroupError::Other { .. } => {}
            other => panic!("expected Other for resource_type-less AlreadyExists, got {other:?}"),
        }
    }

    #[test]
    fn failed_precondition_without_violations_falls_through_to_other() {
        // An empty violation list is a malformed envelope; projecting it to
        // FailedPrecondition with empty subject/type_ would masquerade as a
        // real domain precondition.
        let canonical = RgScope::failed_precondition()
            .with_precondition_violation(
                precondition::HIERARCHY_SUBJECT,
                "would create a cycle",
                precondition::STATE_TYPE,
            )
            .create();
        let malformed = canonical_from_mutated_problem(canonical, |v| {
            v["context"]["violations"] = serde_json::json!([]);
        });
        match ResourceGroupError::from(malformed) {
            ResourceGroupError::Other { .. } => {}
            other => panic!("expected Other for violation-less FailedPrecondition, got {other:?}"),
        }
    }

    #[test]
    fn projection_survives_full_problem_round_trip() {
        // Out-of-process chain: canonical → Problem JSON → Problem →
        // CanonicalError → ResourceGroupError. Pins that an HTTP consumer
        // projecting from the wire gets the same typed variant as an
        // in-process ClientHub caller — exercised on the cycle (hierarchy)
        // dispatch the projection narrows.
        let canonical = RgScope::failed_precondition()
            .with_precondition_violation(
                precondition::HIERARCHY_SUBJECT,
                "would create a cycle",
                precondition::STATE_TYPE,
            )
            .create();

        let bytes = serde_json::to_vec(&Problem::from(canonical)).expect("serialize");
        let restored: Problem = serde_json::from_slice(&bytes).expect("deserialize");
        let restored_canonical = CanonicalError::try_from(restored).expect("reconstruct");

        match ResourceGroupError::from(restored_canonical) {
            ResourceGroupError::FailedPrecondition { subject, type_, .. } => {
                assert_eq!(subject, Subject::Hierarchy);
                assert_eq!(type_, precondition::STATE_TYPE);
            }
            other => panic!("expected FailedPrecondition after round-trip, got {other:?}"),
        }
    }
}
