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
