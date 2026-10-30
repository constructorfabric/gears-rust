use super::*;
use crate::api::rest::dto::EntityKindDto;

#[test]
fn the_kind_filter_accepts_exactly_the_response_spelling() {
    // A new variant fails to compile here until it is listed below.
    let exhaustive = |kind: EntityKind| match kind {
        EntityKind::TypeSchema | EntityKind::Instance => (),
    };
    let kinds = [EntityKind::TypeSchema, EntityKind::Instance];
    kinds.into_iter().for_each(exhaustive);
    for kind in kinds {
        let wire = serde_json::to_value(EntityKindDto::from(kind)).expect("serialize");
        let wire = wire.as_str().expect("a string");
        assert_eq!(parse_kind(wire).ok(), Some(kind), "{wire}");
    }
}

#[test]
fn depth_is_a_plain_positive_decimal_and_zero_is_refused_at_the_wire() {
    assert_eq!(parse_depth("1").ok(), NonZeroU8::new(1));
    assert_eq!(parse_depth("255").ok(), NonZeroU8::new(255));
    for raw in ["0", "00", "", "+5", "-1", "256", "1.0", " 1"] {
        assert!(
            matches!(
                parse_depth(raw),
                Err(CanonicalError::InvalidArgument { .. })
            ),
            "{raw:?} must be refused"
        );
    }
}
