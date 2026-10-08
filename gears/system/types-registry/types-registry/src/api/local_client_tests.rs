use toolkit_canonical_errors::CanonicalError;
use types_registry_sdk::OPERATION_RESOURCE_TYPE;
use uuid::Uuid;

use types_registry_sdk as sdk;

use super::{
    read_back_failed, selection, stored_version, unrepresentable, validator_text, wrong_kind,
};
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
fn a_validator_that_is_not_text_is_refused_not_altered() {
    let text = sdk::Validator::from_bytes(b"\"v1\"".to_vec());
    assert_eq!(validator_text(&text).ok(), Some("\"v1\""));

    let bytes = sdk::Validator::from_bytes(vec![b'"', 0xFF, b'"']);
    assert!(matches!(
        validator_text(&bytes),
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
