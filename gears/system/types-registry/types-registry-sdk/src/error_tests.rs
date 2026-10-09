//! Tests for the [`TypesRegistryError`](super::TypesRegistryError) projection.
//!
//! Two suites:
//!
//! * `wire_vocabulary_round_trip` — pins every wire-string constant the
//!   projection introduces ([`crate::field`], [`crate::precondition`],
//!   [`crate::reason`], [`crate::gts`], [`crate::item_failure`]) to its `Problem`
//!   JSON path. A drift between an SDK constant
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
    use crate::gts::{self, OperationResource, Resource, TypeResource};
    use crate::item_failure::{AdmissionFailure, AdmissionFailureReason, context};
    use crate::precondition::PolicyParameter;
    use crate::reason::aborted::{self, AbortReason};
    use crate::{field, precondition};
    use toolkit_canonical_errors::{CanonicalError, Problem};
    use toolkit_gts::gts_id;

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
    fn operation_resource_type_round_trips_to_context_resource_type() {
        let err = crate::gts::OperationResource::not_found("x")
            .with_resource("x")
            .create();
        assert_eq!(err.resource_type(), Some(gts::OPERATION_RESOURCE_TYPE));
        let json = problem(err);
        assert_eq!(
            json["context"]["resource_type"],
            gts::OPERATION_RESOURCE_TYPE,
            "resource type must round-trip into context.resource_type",
        );
    }

    #[test]
    fn field_reason_constants_round_trip_to_field_violations() {
        for (field_name, reason) in [
            (field::GTS_ID_FIELD, field::INVALID_GTS_ID),
            (field::QUERY_FIELD, field::INVALID_QUERY),
            (field::ENTITY_FIELD, field::VALIDATION_FAILED),
            (field::SELECT_FIELD, field::INVALID_SELECT),
            ("$frobnicate", field::UNSUPPORTED_QUERY_PARAM),
            (field::IDEMPOTENCY_KEY_FIELD, field::INVALID_IDEMPOTENCY_KEY),
            (field::DEADLINE_FIELD, field::INVALID_DEADLINE),
            (field::IDEMPOTENCY_KEY_HEADER, field::VALIDATION_FAILED),
            (field::IF_MATCH_HEADER, field::VALIDATION_FAILED),
            (field::IF_NONE_MATCH_HEADER, field::VALIDATION_FAILED),
            (field::ITEMS_FIELD, field::VALIDATION_FAILED),
            (field::ENTITY_KEY_FIELD, field::VALIDATION_FAILED),
            (field::IF_NONE_MATCH_FIELD, field::VALIDATION_FAILED),
            (field::FORCE_FIELD, field::VALIDATION_FAILED),
            (
                field::EXPECTED_RESOURCE_VERSION_FIELD,
                field::VALIDATION_FAILED,
            ),
            (field::PATTERN_FIELD, field::INVALID_QUERY),
            (field::KIND_FIELD, field::INVALID_QUERY),
            (field::LIFECYCLE_STATUS_FIELD, field::VALIDATION_FAILED),
            (field::DEPTH_FIELD, field::VALIDATION_FAILED),
            (field::LIMIT_FIELD, field::VALIDATION_FAILED),
            (field::CURSOR_FIELD, field::VALIDATION_FAILED),
            (field::TOP_FIELD, field::VALIDATION_FAILED),
            (field::SKIPTOKEN_FIELD, field::VALIDATION_FAILED),
            (field::PAGE_FIELD, field::INVALID_QUERY),
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
            .with_resource(gts_id!(
                "acme.core.events.base.v1~acme.x.events.derived.v2~"
            ))
            .with_precondition_violation(
                gts_id!("acme.core.events.base.v1~"),
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
            json["context"]["violations"][0]["subject"],
            gts_id!("acme.core.events.base.v1~"),
            "parent id must round-trip into violations[].subject",
        );
    }

    #[test]
    fn registration_policy_types_round_trip_to_violations() {
        for (code, typed) in [
            (
                precondition::REGISTRATION_POLICY_ALLOWED_VENDORS,
                PolicyParameter::AllowedVendors,
            ),
            (
                precondition::REGISTRATION_POLICY_TENANT_OWNABLE,
                PolicyParameter::TenantOwnable,
            ),
        ] {
            assert!(code.starts_with(precondition::REGISTRATION_POLICY_PREFIX));
            assert_eq!(PolicyParameter::from_wire(code), Some(typed.clone()));
            assert_eq!(typed.as_wire(), code);
            let err = TypeResource::failed_precondition()
                .with_resource("k")
                .with_precondition_violation(precondition::DEFAULT_REGION, "refused", code)
                .create();
            let json = problem(err);
            assert_eq!(json["context"]["violations"][0]["type"], code);
            assert_eq!(
                json["context"]["violations"][0]["subject"],
                precondition::DEFAULT_REGION
            );
        }
        let future = "REGISTRATION_POLICY_SOMETHING_NEW";
        assert_eq!(
            PolicyParameter::from_wire(future),
            Some(PolicyParameter::Unknown(future.to_owned()))
        );
        assert_eq!(PolicyParameter::from_wire("PARENT_NOT_REGISTERED"), None);
    }

    #[test]
    fn operation_read_failed_round_trips_to_context_reason() {
        let err = OperationResource::aborted("read back failed")
            .with_resource("00000000-0000-0000-0000-000000000009")
            .with_reason(aborted::OPERATION_READ_FAILED)
            .create();
        assert_eq!(
            problem(err)["context"]["reason"],
            aborted::OPERATION_READ_FAILED
        );
        assert_eq!(
            AbortReason::from_wire(aborted::OPERATION_READ_FAILED),
            AbortReason::OperationReadFailed
        );
        assert_eq!(
            AbortReason::OperationReadFailed.as_wire(),
            aborted::OPERATION_READ_FAILED
        );
        assert_eq!(
            AbortReason::from_wire("OTHER"),
            AbortReason::Unknown("OTHER".to_owned())
        );
    }

    #[test]
    fn resource_types_round_trip_through_the_typed_view() {
        for (wire, typed) in [
            (gts::TYPE_RESOURCE_TYPE, Resource::Entity),
            (gts::OPERATION_RESOURCE_TYPE, Resource::Operation),
        ] {
            assert_eq!(Resource::from_wire(wire), typed);
            assert_eq!(typed.as_wire(), wire);
        }
        assert_eq!(
            Resource::from_wire("gts.x.y.z.v1~"),
            Resource::Unknown("gts.x.y.z.v1~".to_owned())
        );
    }

    /// Every admission reason and context name lands at its `Problem` JSON path.
    #[test]
    fn admission_reasons_and_context_names_round_trip_to_violations() {
        use crate::item_failure::reason as r;
        let reasons = [
            r::ACTIVATION_WRITE_SET_EXCEEDED,
            r::ALREADY_EXISTS,
            r::BASELINE_UNRESOLVABLE,
            r::BLOCKED_BY_DEPENDENCY,
            r::BLOCKED_BY_PREDECESSOR,
            r::COMPATIBILITY_UNDECIDABLE,
            r::DEPENDENCY_DELETED,
            r::DEPENDENCY_NOT_FOUND,
            r::DEPENDENT_INVALID,
            r::DIALECT_CHANGED,
            r::ENTITY_DELETED,
            r::FAMILY_KIND_CONFLICT,
            r::HAS_REGISTERED_DEPENDENTS,
            r::FAMILY_SHAPE_CONFLICT,
            r::INCOMPATIBLE_WITH_BASELINE,
            r::INSTANCE_OF_MAJOR_ZERO,
            r::INVALID_DOCUMENT,
            r::INVALID_IDENTIFIER,
            r::INVALID_SCHEMA,
            r::INVALID_VALUE,
            r::MISSING_PREDECESSOR,
            r::NOT_ACTIVE,
            r::PRECONDITION_FAILED,
            r::PUBLISHER_MISMATCH,
            r::RESOLUTION_CLOSURE_EXCEEDED,
            r::RESOLVED_DOCUMENT_TOO_LARGE,
            r::REVALIDATION_EXHAUSTED,
            r::STABLE_DERIVES_FROM_MAJOR_ZERO,
            r::STABLE_REFS_MAJOR_ZERO,
            r::SUPERSEDED,
            r::SYSTEM_FAILURE,
            r::UNPARSABLE_PAYLOAD,
            r::UNREADABLE_VERSION,
            r::UNRECOGNIZED_PAYLOAD,
        ];
        assert_eq!(reasons.len(), 34, "one constant per known reason");
        for code in reasons {
            let typed = AdmissionFailureReason::from_wire(code);
            assert!(
                !matches!(typed, AdmissionFailureReason::Unknown(_)),
                "{code} is not modeled"
            );
            let json = problem(AdmissionFailure::new(typed, "m").into_canonical("k"));
            assert_eq!(json["context"]["violations"][0]["type"], code);
            assert_eq!(json["context"]["violations"][0]["subject"], "k");
        }

        let names = [
            context::DEPENDENCY_ID,
            context::DEPENDENCY_KIND,
            context::DIAGNOSTIC_CODE,
            context::STORED_VERSION,
            context::OFFERED_VERSION,
            context::STORED_PUBLISHER,
            context::OFFERED_PUBLISHER,
        ];
        let failure = names.iter().fold(
            AdmissionFailure::new(AdmissionFailureReason::SystemFailure, "m"),
            |f, name| f.with_context(*name, format!("{name}-value")),
        );
        let json = problem(failure.into_canonical("k"));
        let violations = json["context"]["violations"]
            .as_array()
            .expect("violations array");
        for name in names {
            let entry = violations
                .iter()
                .find(|v| v["type"] == format!("context.{name}"))
                .unwrap_or_else(|| panic!("context.{name} missing: {violations:?}"));
            assert_eq!(entry["subject"], format!("{name}-value"));
            assert_eq!(entry["description"], "");
        }
    }

    /// An item failure survives the out-of-process chain: canonical → Problem JSON →
    /// Problem → canonical → `AdmissionFailure`, unknown reason and context included.
    #[test]
    fn an_admission_failure_survives_a_full_problem_round_trip() {
        for failure in [
            AdmissionFailure::new(AdmissionFailureReason::DependencyNotFound, "absent base")
                .with_context(context::DEPENDENCY_ID, "gts.cf.test.pkg.base.v1~")
                .with_context(context::DEPENDENCY_KIND, "derivation_base"),
            AdmissionFailure::new(
                AdmissionFailureReason::from_wire("a_future_reason"),
                "newer registry",
            )
            .with_context("a_future_context", "value"),
        ] {
            let canonical = failure.clone().into_canonical("gts.cf.test.pkg.thing.v1~");
            let json = problem(canonical.clone());
            assert_eq!(
                json["context"]["violations"][0]["type"],
                failure.reason.as_wire()
            );
            let bytes = serde_json::to_vec(&Problem::from(canonical)).expect("serialize");
            let restored: Problem = serde_json::from_slice(&bytes).expect("deserialize");
            let restored = CanonicalError::try_from(restored).expect("reconstruct");
            assert_eq!(AdmissionFailure::from_canonical(&restored), Some(failure));
        }
    }
}

// ─────────────────────────────────────────────────────────────────────
// Projection: From<CanonicalError> for TypesRegistryError
// ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod projection_tests {
    use super::TypesRegistryError;
    use crate::field::{self, ValidationReason};
    use crate::gts::{self, OperationResource, TypeResource};
    use crate::item_failure::{AdmissionFailure, AdmissionFailureReason, context};
    use crate::precondition::{self, PolicyParameter};
    use crate::reason::aborted;
    use toolkit_canonical_errors::{CanonicalError, Problem};
    use toolkit_gts::gts_id;
    use uuid::Uuid;

    const KEY: &str = "gts.cf.test.pkg.thing.v1~";
    const OP: &str = "00000000-0000-0000-0000-000000000009";

    /// The out-of-process chain every assertion below goes through:
    /// canonical → Problem JSON → Problem → canonical → projection.
    fn over_the_wire(err: CanonicalError) -> TypesRegistryError {
        let bytes = serde_json::to_vec(&Problem::from(err)).expect("serialize");
        let restored: Problem = serde_json::from_slice(&bytes).expect("deserialize");
        TypesRegistryError::from(CanonicalError::try_from(restored).expect("reconstruct"))
    }

    fn read_back(resource_name: &str, reason: &str) -> CanonicalError {
        OperationResource::aborted("accepted, read back failed")
            .with_resource(resource_name)
            .with_reason(reason)
            .create()
    }

    #[test]
    fn an_item_failure_projects_admission_with_its_typed_reason() {
        let failure = AdmissionFailure::new(AdmissionFailureReason::DependencyNotFound, "absent")
            .with_context(context::DEPENDENCY_ID, "gts.cf.test.pkg.base.v1~");
        match over_the_wire(failure.clone().into_canonical(KEY)) {
            TypesRegistryError::Admission { key, failure: got } => {
                assert_eq!(key, KEY);
                assert_eq!(got, failure);
            }
            other => panic!("expected Admission, got {other:?}"),
        }
        let unknown = AdmissionFailure::new(AdmissionFailureReason::from_wire("later"), "m");
        assert!(matches!(
            over_the_wire(unknown.into_canonical(KEY)),
            TypesRegistryError::Admission { failure, .. }
                if failure.reason == AdmissionFailureReason::Unknown("later".to_owned())
        ));
    }

    #[test]
    fn a_policy_refusal_projects_policy_refused_and_never_admission() {
        let refusal = |code: &str| {
            TypeResource::failed_precondition()
                .with_resource(KEY)
                .with_precondition_violation("eu", "vendor not allowed", code)
                .create()
        };
        match over_the_wire(refusal(precondition::REGISTRATION_POLICY_ALLOWED_VENDORS)) {
            TypesRegistryError::PolicyRefused {
                gts_id,
                parameter,
                region,
                detail,
            } => {
                assert_eq!(gts_id, KEY);
                assert_eq!(parameter, PolicyParameter::AllowedVendors);
                assert_eq!(region, "eu");
                assert_eq!(detail, "vendor not allowed");
            }
            other => panic!("expected PolicyRefused, got {other:?}"),
        }
        let default = TypeResource::failed_precondition()
            .with_resource(KEY)
            .with_precondition_violation(
                precondition::DEFAULT_REGION,
                "tenant ownership refused",
                precondition::REGISTRATION_POLICY_TENANT_OWNABLE,
            )
            .create();
        assert!(matches!(
            over_the_wire(default),
            TypesRegistryError::PolicyRefused { region, .. } if region == "<default>"
        ));
        assert!(matches!(
            over_the_wire(refusal("REGISTRATION_POLICY_LATER")),
            TypesRegistryError::PolicyRefused { parameter: PolicyParameter::Unknown(code), .. }
                if code == "REGISTRATION_POLICY_LATER"
        ));
        // Two policy violations, or none naming the candidate, are not the shape.
        let two = TypeResource::failed_precondition()
            .with_resource(KEY)
            .with_precondition_violation(
                "eu",
                "a",
                precondition::REGISTRATION_POLICY_ALLOWED_VENDORS,
            )
            .with_precondition_violation(
                "eu",
                "b",
                precondition::REGISTRATION_POLICY_TENANT_OWNABLE,
            )
            .create();
        assert!(matches!(
            over_the_wire(two),
            TypesRegistryError::Other { .. }
        ));
        let unnamed = TypeResource::failed_precondition()
            .with_precondition_violation(
                "eu",
                "a",
                precondition::REGISTRATION_POLICY_ALLOWED_VENDORS,
            )
            .create();
        assert!(matches!(
            over_the_wire(unnamed),
            TypesRegistryError::Other { .. }
        ));
    }

    #[test]
    fn a_lost_read_back_projects_read_back_failed() {
        match over_the_wire(read_back(OP, aborted::OPERATION_READ_FAILED)) {
            TypesRegistryError::ReadBackFailed { operation_id, .. } => {
                assert_eq!(operation_id, Uuid::from_u128(9));
            }
            other => panic!("expected ReadBackFailed, got {other:?}"),
        }
    }

    #[test]
    fn malformed_read_backs_stay_in_other() {
        for err in [
            read_back("not-a-uuid", aborted::OPERATION_READ_FAILED),
            read_back(OP, "SOMETHING_ELSE"),
            TypeResource::aborted("entity-scoped")
                .with_resource(OP)
                .with_reason(aborted::OPERATION_READ_FAILED)
                .create(),
        ] {
            assert!(
                matches!(over_the_wire(err), TypesRegistryError::Other { .. }),
                "a near miss must keep the full canonical error"
            );
        }
    }

    #[test]
    fn an_operation_deadline_projects_with_or_without_its_id() {
        let named = OperationResource::deadline_exceeded("timed out")
            .with_resource(OP)
            .create();
        assert!(matches!(
            over_the_wire(named),
            TypesRegistryError::DeadlineExceeded { operation_id: Some(id), .. } if id == Uuid::from_u128(9)
        ));
        let unnamed = OperationResource::deadline_exceeded("timed out before acceptance").create();
        assert!(matches!(
            over_the_wire(unnamed),
            TypesRegistryError::DeadlineExceeded {
                operation_id: None,
                ..
            }
        ));
        let malformed = OperationResource::deadline_exceeded("x")
            .with_resource("not-a-uuid")
            .create();
        assert!(matches!(
            over_the_wire(malformed),
            TypesRegistryError::Other { .. }
        ));
        let entity = TypeResource::deadline_exceeded("x").create();
        assert!(matches!(
            over_the_wire(entity),
            TypesRegistryError::Other { .. }
        ));
    }

    #[test]
    fn cancellation_projects_whichever_resource_reported_it() {
        for err in [
            OperationResource::cancelled().create(),
            TypeResource::cancelled().create(),
        ] {
            assert!(matches!(
                over_the_wire(err),
                TypesRegistryError::Cancelled { .. }
            ));
        }
    }

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
            .with_resource(gts_id!("acme.core.events.test.v1~"))
            .create();
        match TypesRegistryError::from(canonical) {
            TypesRegistryError::NotFound {
                resource_type,
                name,
                ..
            } => {
                assert_eq!(resource_type, gts::TYPE_RESOURCE_TYPE);
                assert_eq!(name, gts_id!("acme.core.events.test.v1~"));
            }
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn already_exists_projects_resource_type_and_name() {
        let canonical = TypeResource::already_exists("entity exists")
            .with_resource(gts_id!("acme.core.events.test.v1~"))
            .create();
        match TypesRegistryError::from(canonical) {
            TypesRegistryError::AlreadyExists {
                resource_type,
                name,
                ..
            } => {
                assert_eq!(resource_type, gts::TYPE_RESOURCE_TYPE);
                assert_eq!(name, gts_id!("acme.core.events.test.v1~"));
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
                .with_resource(gts_id!("acme.core.events.test.v1~"))
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
                .with_resource(gts_id!("acme.core.events.test.v1~"))
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
            .with_resource(gts_id!(
                "acme.core.events.base.v1~acme.x.events.derived.v2~"
            ))
            .with_precondition_violation(
                gts_id!("acme.core.events.base.v1~"),
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
                assert_eq!(parent_type_id, gts_id!("acme.core.events.base.v1~"));
                assert_eq!(
                    dependent_id,
                    gts_id!("acme.core.events.base.v1~acme.x.events.derived.v2~")
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
            .with_resource(gts_id!(
                "acme.core.events.base.v1~acme.x.events.derived.v2~"
            ))
            .with_precondition_violation(
                gts_id!("acme.core.events.base.v1~"),
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
                assert_eq!(parent_type_id, gts_id!("acme.core.events.base.v1~"));
                assert_eq!(
                    dependent_id,
                    gts_id!("acme.core.events.base.v1~acme.x.events.derived.v2~")
                );
            }
            other => panic!("expected ParentNotRegistered after round-trip, got {other:?}"),
        }
    }
}
