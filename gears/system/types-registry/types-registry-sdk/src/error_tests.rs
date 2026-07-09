//! Tests for the [`TypesRegistryError`](super::TypesRegistryError) projection.
//!
//! Two suites:
//!
//! * `wire_vocabulary_round_trip` — pins every wire-string constant the
//!   projection introduces ([`crate::field`], [`crate::precondition`],
//!   [`crate::gts`]) to its `Problem` JSON path. A drift between an SDK constant
//!   and the wire trips here.
//! * `projection_tests` — exercises `From<CanonicalError>`, verifying each
//!   canonical category lands on the expected typed variant and that unmodeled
//!   categories preserve the canonical in `Other`.

use super::TypesRegistryError;

// ─────────────────────────────────────────────────────────────────────
// Wire-vocabulary round-trip
// ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod wire_vocabulary_round_trip {
    use crate::gts::{self, TypeResource};
    use crate::{field, precondition};
    use toolkit_canonical_errors::{CanonicalError, Problem};

    fn problem(err: CanonicalError) -> serde_json::Value {
        serde_json::to_value(Problem::from(err)).expect("Problem serializes")
    }

    #[test]
    fn gts_resource_type_round_trips_to_context_resource_type() {
        // Also pins the SDK `TypeResource` marker literal == the const.
        let err = TypeResource::not_found("x").with_resource("x").create();
        let json = problem(err);
        assert_eq!(
            json["context"]["resource_type"],
            gts::TYPE_RESOURCE_TYPE,
            "resource type must round-trip into context.resource_type",
        );
    }

    #[test]
    fn field_reason_constants_round_trip_to_field_violations() {
        for (field_name, reason) in [
            (field::GTS_ID_FIELD, field::INVALID_GTS_ID),
            (field::QUERY_FIELD, field::INVALID_QUERY),
            (field::ENTITY_FIELD, field::VALIDATION_FAILED),
        ] {
            let err = TypeResource::invalid_argument()
                .with_field_violation(field_name, "bad", reason)
                .create();
            let json = problem(err);
            assert_eq!(
                json["context"]["field_violations"][0]["reason"], reason,
                "reason {reason} must round-trip into field_violations[].reason",
            );
            assert_eq!(
                json["context"]["field_violations"][0]["field"], field_name,
                "field {field_name} must round-trip into field_violations[].field",
            );
        }
    }

    #[test]
    fn parent_not_registered_type_round_trips_to_violations() {
        let err = TypeResource::failed_precondition()
            .with_resource("gts.acme.core.events.base.v1~acme.x.derived.v1.0~")
            .with_precondition_violation(
                "gts.acme.core.events.base.v1~",
                "required type-schema is not registered",
                precondition::PARENT_NOT_REGISTERED,
            )
            .create();
        let json = problem(err);
        assert_eq!(
            json["context"]["violations"][0]["type"],
            precondition::PARENT_NOT_REGISTERED,
            "type must round-trip into violations[].type",
        );
        assert_eq!(
            json["context"]["violations"][0]["subject"], "gts.acme.core.events.base.v1~",
            "parent id must round-trip into violations[].subject",
        );
    }
}

// ─────────────────────────────────────────────────────────────────────
// Projection: From<CanonicalError> for TypesRegistryError
// ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod projection_tests {
    use super::TypesRegistryError;
    use crate::field::{self, ValidationReason};
    use crate::gts::{self, TypeResource};
    use crate::precondition;
    use toolkit_canonical_errors::{CanonicalError, Problem};

    #[test]
    fn invalid_argument_projects_typed_validation_reason() {
        let canonical = TypeResource::invalid_argument()
            .with_field_violation(field::GTS_ID_FIELD, "missing vendor", field::INVALID_GTS_ID)
            .create();
        match TypesRegistryError::from(canonical) {
            TypesRegistryError::Validation { issues } => {
                assert_eq!(issues.len(), 1);
                assert_eq!(issues[0].field, field::GTS_ID_FIELD);
                assert_eq!(issues[0].reason, ValidationReason::InvalidGtsId);
                assert_eq!(issues[0].description, "missing vendor");
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[test]
    fn not_found_projects_resource_type_and_name() {
        let canonical = TypeResource::not_found("type schema not found")
            .with_resource("gts.acme.core.events.test.v1~")
            .create();
        match TypesRegistryError::from(canonical) {
            TypesRegistryError::NotFound {
                resource_type,
                name,
                ..
            } => {
                assert_eq!(resource_type, gts::TYPE_RESOURCE_TYPE);
                assert_eq!(name, "gts.acme.core.events.test.v1~");
            }
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn already_exists_projects_resource_type_and_name() {
        let canonical = TypeResource::already_exists("entity exists")
            .with_resource("gts.acme.core.events.test.v1~")
            .create();
        match TypesRegistryError::from(canonical) {
            TypesRegistryError::AlreadyExists {
                resource_type,
                name,
                ..
            } => {
                assert_eq!(resource_type, gts::TYPE_RESOURCE_TYPE);
                assert_eq!(name, "gts.acme.core.events.test.v1~");
            }
            other => panic!("expected AlreadyExists, got {other:?}"),
        }
    }

    /// A `NotFound` envelope that reaches us without `resource_type` metadata
    /// (e.g. a foreign / malformed canonical error) is not a modeled
    /// types-registry `NotFound` — callers dispatch on `TYPE_RESOURCE_TYPE`, so
    /// projecting it to `NotFound { resource_type: "" }` would be a silent lie.
    /// It must fall through to `Other`, preserving the full canonical error.
    #[test]
    fn not_found_without_resource_type_falls_through_to_other() {
        let canonical = malformed_without_resource_type(
            TypeResource::not_found("missing")
                .with_resource("gts.acme.core.events.test.v1~")
                .create(),
        );
        assert!(
            matches!(
                canonical,
                CanonicalError::NotFound {
                    resource_type: None,
                    ..
                }
            ),
            "precondition: the test envelope must lack a resource_type",
        );
        match TypesRegistryError::from(canonical) {
            TypesRegistryError::Other { .. } => {}
            other => panic!("expected Other, got {other:?}"),
        }
    }

    /// Mirror of [`not_found_without_resource_type_falls_through_to_other`] for
    /// the symmetric `AlreadyExists` arm.
    #[test]
    fn already_exists_without_resource_type_falls_through_to_other() {
        let canonical = malformed_without_resource_type(
            TypeResource::already_exists("entity exists")
                .with_resource("gts.acme.core.events.test.v1~")
                .create(),
        );
        assert!(
            matches!(
                canonical,
                CanonicalError::AlreadyExists {
                    resource_type: None,
                    ..
                }
            ),
            "precondition: the test envelope must lack a resource_type",
        );
        match TypesRegistryError::from(canonical) {
            TypesRegistryError::Other { .. } => {}
            other => panic!("expected Other, got {other:?}"),
        }
    }

    /// Strips `resource_type` from a canonical error by round-tripping through
    /// its wire `Problem` (the only public path to a resource-type-less
    /// envelope, since the variants are `#[non_exhaustive]`).
    fn malformed_without_resource_type(err: CanonicalError) -> CanonicalError {
        let mut problem = Problem::from(err);
        problem
            .context
            .as_object_mut()
            .expect("canonical context serializes to a JSON object")
            .remove("resource_type");
        CanonicalError::try_from(problem).expect("problem without resource_type reconstructs")
    }

    #[test]
    fn failed_precondition_projects_parent_not_registered() {
        let canonical = TypeResource::failed_precondition()
            .with_resource("gts.acme.core.events.base.v1~acme.x.derived.v1.0~")
            .with_precondition_violation(
                "gts.acme.core.events.base.v1~",
                "required type-schema is not registered",
                precondition::PARENT_NOT_REGISTERED,
            )
            .create();
        match TypesRegistryError::from(canonical) {
            TypesRegistryError::ParentNotRegistered {
                parent_type_id,
                dependent_id,
                detail,
            } => {
                assert_eq!(parent_type_id, "gts.acme.core.events.base.v1~");
                assert_eq!(
                    dependent_id,
                    "gts.acme.core.events.base.v1~acme.x.derived.v1.0~"
                );
                assert_eq!(detail, "required type-schema is not registered");
            }
            other => panic!("expected ParentNotRegistered, got {other:?}"),
        }
    }

    #[test]
    fn unmodeled_failed_precondition_falls_through_to_other() {
        // A FailedPrecondition whose violation type is NOT
        // PARENT_NOT_REGISTERED is unmodeled — it must preserve the canonical
        // in `Other`, not be mislabeled as ParentNotRegistered.
        let canonical = TypeResource::failed_precondition()
            .with_precondition_violation("some_subject", "future precondition", "SOME_OTHER_TYPE")
            .create();
        match TypesRegistryError::from(canonical) {
            TypesRegistryError::Other {
                canonical: CanonicalError::FailedPrecondition { .. },
            } => {}
            other => panic!("expected Other::FailedPrecondition, got {other:?}"),
        }
    }

    #[test]
    fn service_unavailable_projects_unavailable() {
        let canonical = CanonicalError::service_unavailable().create();
        assert!(matches!(
            TypesRegistryError::from(canonical),
            TypesRegistryError::Unavailable { .. }
        ));
    }

    #[test]
    fn internal_projects_internal() {
        let canonical = CanonicalError::internal("boom").create();
        assert!(matches!(
            TypesRegistryError::from(canonical),
            TypesRegistryError::Internal { .. }
        ));
    }

    #[test]
    fn unmodeled_category_falls_through_to_other() {
        // Types-registry never emits Unauthenticated; it must land in Other
        // with the canonical preserved for inspection.
        let canonical = CanonicalError::unauthenticated()
            .with_reason("SOME_REASON")
            .create();
        match TypesRegistryError::from(canonical) {
            TypesRegistryError::Other {
                canonical: CanonicalError::Unauthenticated { .. },
            } => {}
            other => panic!("expected Other::Unauthenticated, got {other:?}"),
        }
    }

    #[test]
    fn parent_not_registered_survives_full_problem_round_trip() {
        // Out-of-process chain: canonical → Problem JSON → Problem →
        // CanonicalError → TypesRegistryError. Pins that an HTTP consumer
        // projecting from the wire reconstructs the same structured ids as an
        // in-process ClientHub caller — exercised on the lossless
        // parent-not-registered encoding.
        let canonical = TypeResource::failed_precondition()
            .with_resource("gts.acme.core.events.base.v1~acme.x.derived.v1.0~")
            .with_precondition_violation(
                "gts.acme.core.events.base.v1~",
                "required type-schema is not registered",
                precondition::PARENT_NOT_REGISTERED,
            )
            .create();

        let bytes = serde_json::to_vec(&Problem::from(canonical)).expect("serialize");
        let restored: Problem = serde_json::from_slice(&bytes).expect("deserialize");
        let restored_canonical = CanonicalError::try_from(restored).expect("reconstruct");

        match TypesRegistryError::from(restored_canonical) {
            TypesRegistryError::ParentNotRegistered {
                parent_type_id,
                dependent_id,
                ..
            } => {
                assert_eq!(parent_type_id, "gts.acme.core.events.base.v1~");
                assert_eq!(
                    dependent_id,
                    "gts.acme.core.events.base.v1~acme.x.derived.v1.0~"
                );
            }
            other => panic!("expected ParentNotRegistered after round-trip, got {other:?}"),
        }
    }
}
