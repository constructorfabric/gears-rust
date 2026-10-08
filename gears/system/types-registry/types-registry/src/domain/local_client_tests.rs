use toolkit_canonical_errors::CanonicalError;
use types_registry_sdk::OPERATION_RESOURCE_TYPE;
use uuid::Uuid;

use types_registry_sdk as sdk;

use super::{condition, read_back_failed, selection, stored_version, unrepresentable, wrong_kind};
use crate::domain::selection::FieldSelection;

#[test]
fn a_failed_read_back_names_the_accepted_operation_and_says_to_retry_with_the_key() {
    let operation_id = Uuid::from_u128(7);

    let error = read_back_failed(operation_id, "the read failed");

    assert!(matches!(error, CanonicalError::Aborted { .. }), "{error:?}");
    assert_eq!(error.resource_type(), Some(OPERATION_RESOURCE_TYPE));
    assert_eq!(
        error.resource_name(),
        Some(operation_id.to_string().as_str())
    );
    assert!(error.detail().contains("same idempotency key"));
}

#[test]
fn a_precondition_above_the_stored_range_is_refused_not_wrapped() {
    let max = u64::try_from(i64::MAX).expect("fits");
    assert_eq!(
        stored_version("k", max).ok(),
        Some(i64::MAX),
        "the edge is kept"
    );
    let error = stored_version("k", max + 1).expect_err("one above the edge is refused");
    let CanonicalError::InvalidArgument {
        ctx: toolkit_canonical_errors::InvalidArgument::FieldViolations { field_violations },
        ..
    } = &error
    else {
        panic!("not a field violation: {error:?}");
    };
    assert_eq!(
        field_violations[0].field,
        types_registry_sdk::field::EXPECTED_RESOURCE_VERSION_FIELD
    );
    assert_eq!(
        field_violations[0].reason,
        types_registry_sdk::field::VALIDATION_FAILED,
        "the same shape the ladder gives an unusable precondition"
    );
    assert_eq!(
        error.resource_name(),
        Some("k"),
        "the candidate is the resource"
    );
}

#[test]
fn errors_after_an_accepted_submit_name_the_operation() {
    let operation_id = Uuid::from_u128(9);
    let cause = CanonicalError::internal("x").create();

    for error in [
        wrong_kind(operation_id),
        unrepresentable(operation_id, &cause),
    ] {
        assert_eq!(
            error.resource_type(),
            Some(OPERATION_RESOURCE_TYPE),
            "{error:?}"
        );
        assert_eq!(
            error.resource_name(),
            Some(operation_id.to_string().as_str()),
            "{error:?}"
        );
    }
}

#[test]
fn a_validator_is_the_token_text_or_an_unusable_condition() {
    use crate::domain::validator::IfNoneMatch;

    let token = sdk::Validator::from_bytes(b"AQID".to_vec());
    assert!(matches!(
        condition(&token),
        Ok(Some(IfNoneMatch::Validators(tokens))) if tokens == ["AQID"]
    ));

    let bytes = sdk::Validator::from_bytes(vec![0xFF, 0xFE]);
    assert!(
        matches!(condition(&bytes), Ok(None)),
        "not UTF-8: no condition"
    );

    let oversized = sdk::Validator::from_bytes(vec![0xFF; 2048]);
    assert!(matches!(
        condition(&oversized),
        Err(CanonicalError::InvalidArgument { .. })
    ));
}

#[test]
fn a_typed_selection_is_the_one_its_field_names_parse_to() {
    let projections = [
        sdk::Projection::Default,
        sdk::Projection::Select(sdk::FieldSelection::full()),
        sdk::Projection::Select(sdk::FieldSelection::light()),
        sdk::Projection::Select(sdk::FieldSelection::with(&[
            sdk::EntityField::Content,
            sdk::EntityField::Provenance,
        ])),
    ];
    for projection in projections {
        let names: Vec<&str> = projection
            .normalized()
            .fields()
            .map(sdk::EntityField::name)
            .collect();
        let parsed = FieldSelection::parse(&names).expect("SDK names are selectable");
        let typed = selection(&projection);
        assert_eq!(typed, parsed, "{projection:?}");
        assert_eq!(typed.canonical(), parsed.canonical(), "{projection:?}");
    }
}

mod item_failures {
    use sdk::item_failure::{AdmissionFailure, AdmissionFailureReason as Reason, context};

    use super::super::item_parts;
    use super::*;
    use crate::domain::admission::{StoredFailure, UnreadableFailure};
    use crate::domain::enums::OperationItemStatus;
    use crate::domain::key::EntityKey;
    use crate::domain::registry_service::OperationItemRecord;

    const ID: &str = "gts.cf.core.events.test.v1~";

    fn stored(reason: &str) -> StoredFailure {
        StoredFailure {
            reason: reason.to_owned(),
            message: "refused".to_owned(),
            dependency_id: None,
            dependency_kind: None,
            error_code: None,
            operation_id: None,
        }
    }

    /// Through the adapter and back: what an SDK caller decodes from the item error.
    fn decoded(failure: Result<StoredFailure, UnreadableFailure>) -> AdmissionFailure {
        let item = OperationItemRecord {
            key: EntityKey::GtsId(ID.to_owned()),
            status: OperationItemStatus::Failed,
            resource_version: None,
            error: Some(failure),
        };
        let (_, _, _, error) = item_parts(item, Uuid::from_u128(1)).expect("representable");
        AdmissionFailure::from_canonical(&error.expect("a failed item carries its error"))
            .expect("an admission failure")
    }

    #[test]
    fn a_stored_failure_forwards_its_reason_message_and_every_context_entry() {
        let failure = decoded(Ok(StoredFailure {
            dependency_id: Some("gts.cf.core.events.base.v1~".to_owned()),
            dependency_kind: Some("type_schema".to_owned()),
            error_code: Some("storage_timeout".to_owned()),
            ..stored(Reason::DependencyNotFound.as_wire())
        }));

        assert_eq!(failure.reason, Reason::DependencyNotFound);
        assert_eq!(failure.message, "refused");
        assert_eq!(
            failure.context(context::DEPENDENCY_ID),
            Some("gts.cf.core.events.base.v1~")
        );
        assert_eq!(
            failure.context(context::DEPENDENCY_KIND),
            Some("type_schema")
        );
        assert_eq!(
            failure.context(context::DIAGNOSTIC_CODE),
            Some("storage_timeout")
        );
    }

    #[test]
    fn absent_context_is_not_invented() {
        let failure = decoded(Ok(stored(Reason::PreconditionFailed.as_wire())));

        assert_eq!(failure.reason, Reason::PreconditionFailed);
        assert!(failure.context.is_empty(), "{:?}", failure.context);
    }

    #[test]
    fn a_reason_this_build_does_not_know_survives_as_written() {
        let failure = decoded(Ok(stored("added_by_a_newer_writer")));

        assert_eq!(
            failure.reason,
            Reason::Unknown("added_by_a_newer_writer".to_owned())
        );
    }

    #[test]
    fn an_unreadable_record_discloses_only_its_reason() {
        let failure = decoded(Err(UnreadableFailure {
            reason: Reason::UnparsablePayload,
            cause: "expected value at line 1 column 1: {\"secret\"".to_owned(),
        }));

        assert_eq!(failure.reason, Reason::UnparsablePayload);
        assert_eq!(failure.message, "the recorded failure could not be read");
        assert!(failure.context.is_empty(), "{:?}", failure.context);
    }
}
