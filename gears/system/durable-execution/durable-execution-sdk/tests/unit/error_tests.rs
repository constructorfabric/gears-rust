use super::*;
use toolkit_canonical_errors::context::InvalidArgument;

#[test]
fn definition_error_maps_to_invalid_argument() {
    let err = CanonicalError::from(DefinitionError::new("invalid retry policy"));
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
            assert_eq!(field_violations[0].field, "definition");
            assert_eq!(field_violations[0].description, "invalid retry policy");
            assert_eq!(
                field_violations[0].reason,
                crate::reason::INVALID_DEFINITION
            );
        }
        other => panic!("unexpected {other:?}"),
    }
}
