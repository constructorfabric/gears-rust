use super::*;
use toolkit_canonical_errors::context::InvalidArgument;

#[test]
fn definition_error_maps_to_invalid_argument() {
    let err = CanonicalError::from(
        DefinitionError::new(DefinitionErrorKind::InvalidRetryPolicy).for_step("charge"),
    );
    assert_eq!(err.status_code(), 400);
    assert_eq!(
        err.resource_type(),
        Some(crate::gts::DEFINITION_RESOURCE_TYPE)
    );
    match err {
        CanonicalError::InvalidArgument {
            ctx: InvalidArgument::FieldViolations { field_violations },
            ..
        } => {
            assert_eq!(field_violations.len(), 1);
            assert_eq!(field_violations[0].field, "activities.charge");
            assert_eq!(
                field_violations[0].description,
                DefinitionErrorKind::InvalidRetryPolicy.to_string()
            );
            assert_eq!(
                field_violations[0].reason,
                crate::reason::INVALID_DEFINITION
            );
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn definition_error_display_names_the_rule_and_step() {
    let error = DefinitionError::new(DefinitionErrorKind::DuplicateActivityId).for_step("charge");
    assert_eq!(error.kind(), DefinitionErrorKind::DuplicateActivityId);
    assert_eq!(error.step(), Some("charge"));
    assert_eq!(
        error.to_string(),
        "invalid execution definition: duplicate activity id (step `charge`)"
    );
    assert_eq!(
        DefinitionError::new(DefinitionErrorKind::Encoding).to_string(),
        "invalid execution definition: contract cannot be encoded"
    );
}
